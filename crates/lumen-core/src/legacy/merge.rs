//! Late-row merge: rows an old binary wrote into the old ledger after the
//! migration are carried into the new one, any number of times, without
//! losing or duplicating a row.
//!
//! The old ledger stays where it was (as the dated backup, or as a recreated
//! file at the old path) and an old daemon or hook may keep writing to it. At
//! every start the app calls [`merge_late`].
//!
//! Idempotence is by position, not by content, with a content reconciliation
//! only when the source's rowids restarted (see `carry_positional`). The ledger has no unique key on
//! `read_events` or `faults` (two sessions, two subagents or two ranged reads
//! of one file can produce rows that agree on every column but their rowid),
//! so matching rows by what they contain would collapse genuine rows. Instead
//! the new ledger records, in `legacy_merge`, the highest source rowid carried
//! per source and table, in the same transaction as the rows themselves: a
//! crash before the commit redoes the batch, a crash after it skips the batch.
//! Tables with a primary key use `INSERT OR IGNORE`; their keys are identities.
//!
//! The fault spool cannot join that transaction. It is copied by intent: the
//! range and the destination length are committed first, then the bytes are
//! appended, then the progress is committed. After a crash the destination is
//! checked at the recorded position before anything is appended again. The one
//! case that is not exact: another process appending to the destination at the
//! very moment of a crash that tore an append, which can leave a partial line
//! of ours before theirs. The destination is never truncated.
//!
//! Columns are matched by name on both sides. A 1.8.0 backup lacks columns the
//! new ledger has (they take its defaults); a source with columns the new
//! ledger lacks is reported and merged on the common ones.

use super::Marker;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Tables with a natural key: `INSERT OR IGNORE`.
pub const KEYED_TABLES: [&str; 5] = [
    "turns",
    "calibration",
    "session_events",
    "intercept_blocks",
    "transcript_scans",
];

/// Tables with no key: carried by rowid position.
pub const POSITIONAL_TABLES: [&str; 2] = ["read_events", "faults"];

/// How much of a spool's head says it is the same spool.
const SIG_BYTES: u64 = 256;

/// What happened to one source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceReport {
    pub path: PathBuf,
    /// Why nothing was merged: missing, not a ledger, the same file as the
    /// new ledger, or an error (the transaction was rolled back).
    pub skipped: Option<String>,
    /// Rows inserted, per table.
    pub merged: BTreeMap<String, usize>,
    /// Bytes appended to the fault spool.
    pub spool_bytes: u64,
    /// Things worth reporting that did not stop the merge (a newer schema).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub sources: Vec<SourceReport>,
}

impl MergeReport {
    pub fn rows(&self) -> usize {
        self.sources.iter().flat_map(|s| s.merged.values()).sum()
    }
}

/// Merge what `sources` (ledger files) hold and `new_db` lacks.
///
/// `marker` seeds the progress of the first source that is a ledger, the one
/// the migration copied: rows up to its `hwm` and bytes up to its
/// `spool_offset` came with the copy. From then on the progress lives in the
/// new ledger and the marker is not written. A source that is a symlink to an
/// earlier one is the same file and counts once.
///
/// `Err` only when the new ledger cannot be opened. A bad source is a
/// [`SourceReport::skipped`], never an error.
pub fn merge_late(
    new_db: &Path,
    sources: &[&Path],
    marker: &Marker,
) -> Result<MergeReport, String> {
    merge_impl(new_db, sources, marker, false)
}

fn merge_impl(
    new_db: &Path,
    sources: &[&Path],
    marker: &Marker,
    stop_after_commit: bool,
) -> Result<MergeReport, String> {
    let dir = new_db
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", new_db.display()))?;
    let conn = Connection::open_with_flags(
        new_db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("cannot open {}: {e}", new_db.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;

    let own = std::fs::canonicalize(new_db).ok();
    let mut seen: BTreeSet<PathBuf> = own.into_iter().collect();
    let mut report = MergeReport::default();
    let mut primary_done = false;

    for &source in sources {
        let mut sr = SourceReport {
            path: source.to_path_buf(),
            ..Default::default()
        };
        let canon = match std::fs::canonicalize(source) {
            Ok(c) => c,
            Err(e) => {
                sr.skipped = Some(format!("missing: {e}"));
                report.sources.push(sr);
                continue;
            }
        };
        if !seen.insert(canon.clone()) {
            sr.skipped = Some("same file as the new ledger or an earlier source".into());
            report.sources.push(sr);
            continue;
        }
        if let Err(why) = check_ledger(source) {
            sr.skipped = Some(why);
            report.sources.push(sr);
            continue;
        }
        let key = canon.to_string_lossy().into_owned();
        let seed = if primary_done { None } else { Some(marker) };
        primary_done = true;
        match merge_one(&conn, source, &key, seed, &mut sr) {
            Ok(()) => {
                if stop_after_commit {
                    report.sources.push(sr);
                    return Ok(report);
                }
                match merge_spool(&conn, source, &key, dir, seed.map(|m| m.spool_offset)) {
                    Ok(appended) => sr.spool_bytes = appended,
                    Err(e) => sr.notes.push(format!("fault spool not merged: {e}")),
                }
            }
            Err(why) => sr.skipped = Some(why),
        }
        report.sources.push(sr);
    }
    Ok(report)
}

/// A readable SQLite file with at least one of Lumen's tables.
fn check_ledger(source: &Path) -> Result<(), String> {
    let c = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("not a ledger: {e}"))?;
    let names: Vec<String> = (|| {
        let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
        st.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
    })()
    .map_err(|e| format!("not a ledger: {e}"))?;
    let known = KEYED_TABLES.iter().chain(POSITIONAL_TABLES.iter());
    if known.into_iter().any(|t| names.iter().any(|n| n == t)) {
        Ok(())
    } else {
        Err("not a ledger: none of its tables".into())
    }
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// A `file:` URI that opens the path read-only. `ATTACH` takes a plain path
/// read-write, so the URI is the only way to refuse writes to the source.
///
/// Takes the path as text so the Windows forms can be tested anywhere. A
/// verbatim drive path (`\\?\C:\x`) is the drive path; a UNC path (`\\host\share`,
/// `\\?\UNC\host\share`) is refused, because SQLite's URI form has no portable
/// spelling for it.
pub(super) fn ro_uri(path: &str) -> Result<String, String> {
    let s = path.strip_prefix(r"\\?\").unwrap_or(path);
    let verbatim = s.len() != path.len();
    if s.starts_with(r"\\")
        || (verbatim && s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\")))
    {
        return Err(format!("a network (UNC) path cannot be attached: {path}"));
    }
    let s = s.replace('\\', "/");
    let mut out = String::from("file:");
    if !s.starts_with('/') {
        out.push('/'); // a Windows drive path: file:/C:/...
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out.push_str("?mode=ro");
    Ok(out)
}

pub(super) fn columns(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(&format!("PRAGMA {schema}.table_info({})", quote(table)))?;
    st.query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()
}

/// The progress of `source` for `tbl`: `(hwm, sig)`.
fn progress(
    conn: &Connection,
    source: &str,
    tbl: &str,
) -> Result<Option<(i64, Option<String>)>, String> {
    conn.query_row(
        "SELECT hwm, sig FROM legacy_merge WHERE source = ?1 AND tbl = ?2",
        params![source, tbl],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(|e| format!("legacy_merge: {e}"))
}

fn set_progress(
    conn: &Connection,
    source: &str,
    tbl: &str,
    hwm: i64,
    sig: Option<&str>,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO legacy_merge (source, tbl, hwm, sig) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (source, tbl) DO UPDATE SET hwm = excluded.hwm, sig = excluded.sig",
        params![source, tbl, hwm, sig],
    )
    .map(|_| ())
    .map_err(|e| format!("legacy_merge: {e}"))
}

/// One source, one transaction: the rows and their progress commit together.
fn merge_one(
    conn: &Connection,
    source: &Path,
    key: &str,
    seed: Option<&Marker>,
    sr: &mut SourceReport,
) -> Result<(), String> {
    let uri = ro_uri(&source.to_string_lossy())?;
    conn.execute("ATTACH DATABASE ?1 AS src", params![uri])
        .map_err(|e| format!("cannot attach: {e}"))?;
    let outcome = (|| -> Result<(), String> {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        match copy_rows(conn, key, seed, sr) {
            Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string()),
            Err(e) => {
                // The error is the report; a failed rollback leaves nothing to add.
                if conn.execute_batch("ROLLBACK").is_err() {
                    return Err(format!("{e} (and the rollback failed)"));
                }
                sr.merged.clear();
                Err(e)
            }
        }
    })();
    // A failed detach leaves the attachment on a connection that is dropped next.
    if let Err(e) = conn.execute_batch("DETACH DATABASE src") {
        return outcome.and_then(|()| Err(format!("cannot detach: {e}")));
    }
    outcome
}

fn copy_rows(
    conn: &Connection,
    key: &str,
    seed: Option<&Marker>,
    sr: &mut SourceReport,
) -> Result<(), String> {
    let tables = KEYED_TABLES
        .iter()
        .map(|t| (*t, false))
        .chain(POSITIONAL_TABLES.iter().map(|t| (*t, true)));
    for (table, positional) in tables {
        let new_cols = columns(conn, "main", table).map_err(|e| e.to_string())?;
        let src_cols = columns(conn, "src", table).map_err(|e| e.to_string())?;
        if new_cols.is_empty() || src_cols.is_empty() {
            continue;
        }
        let extra: Vec<&str> = src_cols
            .iter()
            .filter(|c| !new_cols.contains(c))
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            sr.notes.push(format!(
                "{table}: the source is on a newer schema, columns not carried: {}",
                extra.join(", ")
            ));
        }
        let common: Vec<&String> = new_cols.iter().filter(|c| src_cols.contains(c)).collect();
        if common.is_empty() {
            continue;
        }
        let col_list = common
            .iter()
            .map(|c| quote(c))
            .collect::<Vec<_>>()
            .join(", ");
        let t = quote(table);
        let n = if positional {
            let names: Vec<&str> = common.iter().map(|c| c.as_str()).collect();
            carry_positional(conn, key, table, seed, &src_cols, &names, sr)?
        } else {
            conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO main.{t} ({col_list}) SELECT {col_list} FROM src.{t}"
                ),
                [],
            )
            .map_err(|e| format!("{table}: {e}"))?
        };
        if n > 0 {
            sr.merged.insert(table.to_string(), n);
        }
    }
    Ok(())
}

/// SQL for one string that is equal for two rows exactly when every listed column is equal
/// (NULL equal to NULL): each value quoted, so no value can be mistaken for the separator.
fn key_expr(alias: &str, cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("quote({alias}{})", quote(c)))
        .collect::<Vec<_>>()
        .join(" || ',' || ")
}

/// A digest of the source row at `id`, over all its columns; `None` when there is no such row.
fn row_sig(
    conn: &Connection,
    table: &str,
    src_cols: &[String],
    id: i64,
) -> Result<Option<String>, String> {
    row_sig_in(conn, "src", table, src_cols, id)
}

/// [`row_sig`] in the named schema; the migration records these in the marker.
pub(super) fn row_sig_in(
    conn: &Connection,
    schema: &str,
    table: &str,
    cols: &[String],
    id: i64,
) -> Result<Option<String>, String> {
    let cols: Vec<&str> = cols.iter().map(String::as_str).collect();
    let key: Option<String> = conn
        .query_row(
            &format!(
                "SELECT {} FROM {}.{} WHERE rowid = ?1",
                key_expr("", &cols),
                schema,
                quote(table)
            ),
            params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("{table}: {e}"))?;
    Ok(key.map(|k| {
        Sha256::digest(k.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }))
}

/// Carry the rows of a table with no key.
///
/// Normally by position: rowids past the recorded high-water mark, nothing
/// kept per row. The source has *rewound* when its row at the mark is not the
/// row recorded there, or its highest rowid is below the mark: rowids restart
/// after `DELETE` (a table with no INTEGER PRIMARY KEY numbers new rows
/// max+1), are reused after the top rows go, and can be renumbered by `VACUUM`.
/// Then position says nothing, and the table is reconciled against the new
/// ledger's own rows: for each distinct row (the columns both ledgers have, so
/// all but the rowid) the source holds `k` copies, the new ledger holds `c`,
/// and `k - c` more are inserted, in one statement. Nothing that survived is
/// repeated and nothing new is dropped.
///
/// Why comparing against all of the new ledger is sound: one event is written
/// by one binary, so a row the new binaries wrote cannot equal, in every column
/// (`ts` to the second, path, token counts, session, writer), a row an old
/// binary wrote for another event; the only rows of the new ledger equal to a
/// source row are that row's own earlier copies (the migration's, or a previous
/// merge's). The one assumption: a new source row identical in every column to
/// a row already in the new ledger is the same event. That is judged only after
/// a rewind and costs at most that row. Source copies are counted, so a repeated
/// identical event the old binary wrote twice is still carried twice; a row is
/// skipped only when the new ledger holds more copies than the source, which
/// needs the old ledger to delete a row and then write an identical one in the
/// same second.
fn carry_positional(
    conn: &Connection,
    key: &str,
    table: &str,
    seed: Option<&Marker>,
    src_cols: &[String],
    common: &[&str],
    sr: &mut SourceReport,
) -> Result<usize, String> {
    let t = quote(table);
    let col_list = common
        .iter()
        .map(|c| quote(c))
        .collect::<Vec<_>>()
        .join(", ");
    // An empty table has nothing past any mark.
    let top: i64 = conn
        .query_row(
            &format!("SELECT coalesce(max(rowid), 0) FROM src.{t}"),
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("{table}: {e}"))?;
    // Progress in this ledger wins; the marker's mark (rows the copy carried) seeds the first run.
    let (held, sig) = if let Some(p) = progress(conn, key, table)? {
        p
    } else if let Some(h) = seed.and_then(|m| m.hwm.get(table)) {
        (*h, None)
    } else {
        (0, None)
    };
    let rewound = held > 0
        && (top < held
            || match &sig {
                Some(s) => row_sig(conn, table, src_cols, held)?.as_ref() != Some(s),
                None => row_sig(conn, table, src_cols, held)?.is_none(),
            });

    let n = if rewound {
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS lumen_merge_have (k TEXT PRIMARY KEY, c INTEGER NOT NULL) \
             WITHOUT ROWID; DELETE FROM lumen_merge_have",
        )
        .map_err(|e| e.to_string())?;
        let ours = key_expr("", common);
        conn.execute(
            &format!(
                "INSERT INTO lumen_merge_have SELECT {ours}, count(*) FROM main.{t} GROUP BY 1"
            ),
            [],
        )
        .map_err(|e| format!("{table}: {e}"))?;
        let theirs = key_expr("", common);
        let n = conn
            .execute(
                &format!(
                    "INSERT INTO main.{t} ({col_list}) SELECT {col_list} FROM ( \
                       SELECT {col_list}, {theirs} AS k, \
                              row_number() OVER (PARTITION BY {theirs} ORDER BY rowid) AS rn \
                       FROM src.{t}) AS a \
                     WHERE rn > coalesce((SELECT c FROM lumen_merge_have WHERE k = a.k), 0) \
                     ORDER BY rn"
                ),
                [],
            )
            .map_err(|e| format!("{table}: {e}"))?;
        conn.execute_batch("DROP TABLE lumen_merge_have")
            .map_err(|e| e.to_string())?;
        sr.notes.push(format!(
            "{table}: the source's rowids restarted, reconciled by content"
        ));
        n
    } else {
        conn.execute(
            &format!(
                "INSERT INTO main.{t} ({col_list}) SELECT {col_list} FROM src.{t} \
                 WHERE rowid > ?1 AND rowid <= ?2 ORDER BY rowid"
            ),
            params![held, top],
        )
        .map_err(|e| format!("{table}: {e}"))?
    };
    let new_sig = if top > 0 {
        row_sig(conn, table, src_cols, top)?
    } else {
        None
    };
    set_progress(conn, key, table, top, new_sig.as_deref())?;
    Ok(n)
}

fn sig_of(path: &Path, offset: u64) -> Result<String, String> {
    let n = offset.min(SIG_BYTES) as usize;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]).map_err(|e| e.to_string())? {
            0 => break,
            k => got += k,
        }
    }
    buf.truncate(got);
    Ok(Sha256::digest(&buf)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn file_len(path: &Path) -> Result<Option<u64>, String> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Length of `path`, 0 when it does not exist.
fn file_len0(path: &Path) -> Result<u64, String> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.to_string()),
    }
}

fn read_range(path: &Path, from: u64, to: u64) -> Result<Option<Vec<u8>>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if to > len || from > to {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(Some(buf))
}

fn append(dst: &Path, bytes: &[u8]) -> Result<u64, String> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dst)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(bytes.len() as u64)
}

/// Make sure `chunk` is in `dst` from byte `before`: all of it, the rest of
/// it after a torn append, or nothing more when it is already there.
fn finish_append(dst: &Path, before: u64, chunk: &[u8]) -> Result<u64, String> {
    let dlen = file_len0(dst)?;
    if dlen <= before {
        return append(dst, chunk);
    }
    let have = read_range(dst, before, dlen.min(before + chunk.len() as u64))?
        .ok_or("the fault spool shrank while it was read")?;
    if chunk.starts_with(&have) {
        append(dst, &chunk[have.len()..])
    } else {
        // Not ours at that position: it was never appended.
        append(dst, chunk)
    }
}

/// Copy the spool bytes past the recorded offset, at line boundaries, by
/// intent (see the module docs). Returns the bytes appended.
fn merge_spool(
    conn: &Connection,
    source: &Path,
    key: &str,
    new_dir: &Path,
    seed: Option<u64>,
) -> Result<u64, String> {
    let Some(src_dir) = source.parent() else {
        return Ok(0);
    };
    let src_spool = src_dir.join("faults.jsonl");
    let dst_spool = new_dir.join("faults.jsonl");
    if file_len(&src_spool)?.is_none() {
        return Ok(0);
    }
    if std::fs::canonicalize(&src_spool).ok() == std::fs::canonicalize(&dst_spool).ok() {
        return Ok(0);
    }
    let tx = |sql: &str| conn.execute_batch(sql).map_err(|e| e.to_string());
    let mut appended = 0;

    // A copy a crash cut short: finish it exactly.
    let intent: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT src_from, src_to, dst_before FROM legacy_spool_intent WHERE source = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((from, to, before)) = intent {
        let redone = read_range(&src_spool, from as u64, to as u64)?;
        tx("BEGIN IMMEDIATE")?;
        let r = (|| {
            if let Some(chunk) = &redone {
                appended += finish_append(&dst_spool, before as u64, chunk)?;
                set_progress(
                    conn,
                    key,
                    "spool",
                    to,
                    Some(&sig_of(&src_spool, to as u64)?),
                )?;
            }
            conn.execute(
                "DELETE FROM legacy_spool_intent WHERE source = ?1",
                params![key],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })();
        finish(conn, r)?;
    }

    let (mut offset, sig) = if let Some((h, s)) = progress(conn, key, "spool")? {
        (h as u64, s)
    } else if let Some(o) = seed {
        (o, None)
    } else {
        (0, None)
    };
    let len = match file_len(&src_spool)? {
        Some(l) => l,
        None => return Ok(appended),
    };
    // Shorter than the offset, or a different head: drained and restarted.
    if offset > len {
        offset = 0;
    } else if offset > 0
        && let Some(s) = &sig
        && *s != sig_of(&src_spool, offset)?
    {
        offset = 0;
    }
    let Some(raw) = read_range(&src_spool, offset, len)? else {
        return Ok(appended);
    };
    // Whole lines only; a half-written last line waits for the next run.
    let Some(last_nl) = raw.iter().rposition(|b| *b == b'\n') else {
        return Ok(appended);
    };
    let chunk = &raw[..=last_nl];
    let to = offset + chunk.len() as u64;
    let before = file_len0(&dst_spool)?;

    tx("BEGIN IMMEDIATE")?;
    let r = conn
        .execute(
            "INSERT OR REPLACE INTO legacy_spool_intent (source, src_from, src_to, dst_before) \
             VALUES (?1, ?2, ?3, ?4)",
            params![key, offset as i64, to as i64, before as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    finish(conn, r)?;

    appended += append(&dst_spool, chunk)?;

    tx("BEGIN IMMEDIATE")?;
    let r = (|| {
        set_progress(
            conn,
            key,
            "spool",
            to as i64,
            Some(&sig_of(&src_spool, to)?),
        )?;
        conn.execute(
            "DELETE FROM legacy_spool_intent WHERE source = ?1",
            params![key],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })();
    finish(conn, r)?;
    Ok(appended)
}

/// Commit on `Ok`, roll back on `Err`.
fn finish(conn: &Connection, r: Result<(), String>) -> Result<(), String> {
    match r {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string()),
        Err(e) => {
            if conn.execute_batch("ROLLBACK").is_err() {
                return Err(format!("{e} (and the rollback failed)"));
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
    const V190: &str = include_str!("../../tests/fixtures/ledger_schema_v1.9.0.sql");

    fn marker() -> Marker {
        Marker {
            source: "old".into(),
            backup: "b".into(),
            state: "copied".into(),
            hwm: BTreeMap::new(),
            spool_offset: 0,
        }
    }

    /// The current schema, as the daemon builds it.
    fn new_ledger(dir: &Path) -> PathBuf {
        let p = dir.join("lumen.db");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(crate::schema::DDL).unwrap();
        for m in crate::schema::MIGRATIONS {
            c.execute_batch(m).ok(); // "duplicate column" is the expected answer on a current schema
        }
        p
    }

    fn old_ledger(dir: &Path, ddl: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("lumen.db");
        Connection::open(&p).unwrap().execute_batch(ddl).unwrap();
        p
    }

    fn q(p: &Path, sql: &str) {
        Connection::open(p).unwrap().execute_batch(sql).unwrap();
    }

    fn count(p: &Path, table: &str) -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn add_read(p: &Path, ts: &str, path: &str, via: &str, req: Option<&str>) {
        let req = req.map_or("NULL".to_string(), |r| format!("'{r}'"));
        q(
            p,
            &format!(
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                 saved_tokens, routed_via, req_key) VALUES ('{ts}', 'Read', '{path}', 1, 2, 1, \
                 '{via}', {req})"
            ),
        );
    }

    fn add_turn(p: &Path, id: &str) {
        q(
            p,
            &format!(
                "INSERT INTO turns (message_id, session_id, ts, input_tokens, output_tokens, \
                 cache_read_input_tokens, cache_creation_input_tokens) \
                 VALUES ('{id}', 's', '2026-01-01T00:00:00Z', 1, 2, 3, 4)"
            ),
        );
    }

    fn add_fault(p: &Path, ts: &str, kind: &str) {
        q(
            p,
            &format!("INSERT INTO faults (ts, kind, session_id) VALUES ('{ts}', '{kind}', 's')"),
        );
    }

    fn seed(p: &Path) {
        add_turn(p, "m1");
        add_turn(p, "m2");
        q(p, "INSERT INTO calibration VALUES ('m1', 10, 12)");
        q(
            p,
            "INSERT INTO session_events VALUES ('s', 'compaction', 'u1', 5, NULL)",
        );
        add_read(p, "t1", "/a", "builtin_read", None);
        add_read(p, "t2", "/b", "smart_read", Some("k1"));
        add_fault(p, "t1", "decline");
    }

    fn sha(p: &Path) -> Vec<u8> {
        Sha256::digest(std::fs::read(p).unwrap()).to_vec()
    }

    struct Env {
        _t: TempDir,
        new_dir: PathBuf,
        new_db: PathBuf,
        old_db: PathBuf,
    }

    impl Env {
        fn src_spool(&self) -> PathBuf {
            self.old_db.parent().unwrap().join("faults.jsonl")
        }
        fn dst_spool(&self) -> PathBuf {
            self.new_dir.join("faults.jsonl")
        }
        fn dst(&self) -> String {
            std::fs::read_to_string(self.dst_spool()).unwrap_or_default()
        }
        fn merge(&self) -> MergeReport {
            merge_late(&self.new_db, &[&self.old_db], &marker()).unwrap()
        }
    }

    fn env(ddl: &str) -> Env {
        let t = TempDir::new().unwrap();
        let new_dir = t.path().join("new");
        std::fs::create_dir_all(&new_dir).unwrap();
        let new_db = new_ledger(&new_dir);
        let old_db = old_ledger(&t.path().join("old"), ddl);
        Env {
            _t: t,
            new_dir,
            new_db,
            old_db,
        }
    }

    fn key(e: &Env) -> String {
        std::fs::canonicalize(&e.old_db)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn rows_written_after_the_migration_are_merged_from_both_schemas() {
        for ddl in [V180, V190] {
            let e = env(ddl);
            seed(&e.old_db);
            let r = e.merge();
            assert_eq!(r.sources[0].skipped, None);
            assert_eq!(r.rows(), 2 + 1 + 1 + 2 + 1);
            for (t, n) in [
                ("turns", 2),
                ("calibration", 1),
                ("session_events", 1),
                ("read_events", 2),
                ("faults", 1),
            ] {
                assert_eq!(count(&e.new_db, t), n, "{t}");
            }
            // An old binary writes more; only the new rows come across.
            add_turn(&e.old_db, "m3");
            add_read(&e.old_db, "t3", "/c", "builtin_read", None);
            let r = e.merge();
            assert_eq!(r.sources[0].merged["read_events"], 1);
            assert_eq!(count(&e.new_db, "turns"), 3);
            assert_eq!(count(&e.new_db, "read_events"), 3);
        }
    }

    /// Rows that agree on every column but the rowid are different rows: the ledger has no
    /// unique key on read_events, and a second is the grain of `ts`.
    #[test]
    fn reads_that_look_alike_are_all_kept() {
        let e = env(V190);
        add_read(&e.new_db, "t1", "/a", "builtin_read", None); // N's own row
        for (sess, agent, lines) in [("s1", None, 10), ("s2", None, 10), ("s1", Some("sub"), 500)] {
            q(
                &e.old_db,
                &format!(
                    "INSERT INTO read_events (ts, tool, path, lines, tokens_returned, full_tokens, \
                     saved_tokens, routed_via, session_id, agent_id) VALUES ('t1', 'Read', '/a', \
                     {lines}, 1, 2, 1, 'builtin_read', '{sess}', {})",
                    agent.map_or("NULL".to_string(), |a| format!("'{a}'"))
                ),
            );
        }
        add_fault(&e.old_db, "t1", "decline");
        add_fault(&e.old_db, "t1", "decline");
        e.merge();
        assert_eq!(count(&e.new_db, "read_events"), 4);
        assert_eq!(count(&e.new_db, "faults"), 2);
        e.merge();
        assert_eq!(count(&e.new_db, "read_events"), 4);
        assert_eq!(count(&e.new_db, "faults"), 2);
    }

    #[test]
    fn a_rerun_with_nothing_new_adds_nothing() {
        let e = env(V190);
        seed(&e.old_db);
        e.merge();
        let r = e.merge();
        assert_eq!(r.rows(), 0, "{r:?}");
        for t in [
            "turns",
            "read_events",
            "faults",
            "calibration",
            "session_events",
        ] {
            assert_eq!(count(&e.new_db, t), count(&e.old_db, t), "{t}");
        }
    }

    #[test]
    fn the_marker_seeds_the_first_run_only() {
        let e = env(V190);
        for i in 0..3 {
            add_read(&e.old_db, &format!("t{i}"), "/a", "builtin_read", None);
        }
        let mut m = marker();
        m.hwm.insert("read_events".into(), 2); // the copy carried two
        merge_late(&e.new_db, &[&e.old_db], &m).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 1, "only the third");
        // The ledger now holds the progress; a stale marker cannot move it back.
        merge_late(&e.new_db, &[&e.old_db], &marker()).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 1);
        add_read(&e.old_db, "t9", "/a", "builtin_read", None);
        merge_late(&e.new_db, &[&e.old_db], &marker()).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 2);
        let h: i64 = Connection::open(&e.new_db)
            .unwrap()
            .query_row(
                "SELECT hwm FROM legacy_merge WHERE source = ?1 AND tbl = 'read_events'",
                [key(&e)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(h, 4);
    }

    #[test]
    fn a_kill_after_the_commit_skips_the_batch_and_the_next_batch_comes() {
        let e = env(V180);
        seed(&e.old_db);
        // The process dies right after the first batch's commit.
        merge_impl(&e.new_db, &[&e.old_db], &marker(), true).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 2, "the commit stood");
        add_read(&e.old_db, "t9", "/z", "builtin_read", None);
        let r = e.merge();
        assert_eq!(r.sources[0].merged["read_events"], 1, "{r:?}");
        assert_eq!(count(&e.new_db, "read_events"), 3);
        assert_eq!(count(&e.new_db, "turns"), 2);
        assert_eq!(count(&e.new_db, "faults"), 1);
    }

    #[test]
    fn a_kill_before_the_commit_redoes_the_whole_batch() {
        // The source lets a NULL ts in; the new ledger refuses it, after read_events went in.
        let e = env(
            "CREATE TABLE read_events (ts TEXT NOT NULL, tool TEXT NOT NULL, path TEXT NOT NULL, \
                tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL, \
                saved_tokens INTEGER NOT NULL, routed_via TEXT NOT NULL); \
             CREATE TABLE faults (ts TEXT, kind TEXT NOT NULL);",
        );
        q(
            &e.old_db,
            "INSERT INTO read_events VALUES ('t', 'Read', '/a', 1, 2, 1, 'builtin_read'); \
             INSERT INTO faults (ts, kind) VALUES (NULL, 'k')",
        );
        let r = e.merge();
        assert!(r.sources[0].skipped.is_some(), "{r:?}");
        assert_eq!(count(&e.new_db, "read_events"), 0, "rolled back");
        assert_eq!(
            count(&e.new_db, "legacy_merge"),
            0,
            "progress rolled back with it"
        );
        q(&e.old_db, "DELETE FROM faults");
        let r = e.merge();
        assert_eq!(r.sources[0].skipped, None);
        assert_eq!(count(&e.new_db, "read_events"), 1, "once");
    }

    /// Add one row to a keyless table, distinguished by `tag`.
    fn add(table: &str, p: &Path, tag: &str) {
        match table {
            "read_events" => add_read(p, tag, "/a", "builtin_read", None),
            _ => add_fault(p, tag, "decline"),
        }
    }

    #[test]
    fn a_source_whose_rowids_restarted_loses_and_repeats_nothing() {
        for table in ["read_events", "faults"] {
            // The documented way to clear measurements, then new rows at rowid 1, 2.
            let e = env(V190);
            for t in ["t1", "t2", "t3"] {
                add(table, &e.old_db, t);
            }
            e.merge();
            q(&e.old_db, &format!("DELETE FROM {table}"));
            add(table, &e.old_db, "t4");
            add(table, &e.old_db, "t5");
            e.merge();
            assert_eq!(count(&e.new_db, table), 5, "{table}");
            e.merge();
            assert_eq!(count(&e.new_db, table), 5, "{table}: a rerun adds nothing");
            add(table, &e.old_db, "t6");
            e.merge();
            assert_eq!(count(&e.new_db, table), 6, "{table}");
        }
    }

    #[test]
    fn deleting_the_top_rows_and_reusing_their_rowids_loses_and_repeats_nothing() {
        for table in ["read_events", "faults"] {
            let e = env(V190);
            for t in ["a", "b", "c"] {
                add(table, &e.old_db, t);
            }
            e.merge();
            // The top row goes and a new one takes its rowid.
            q(&e.old_db, &format!("DELETE FROM {table} WHERE rowid = 3"));
            add(table, &e.old_db, "d");
            e.merge();
            assert_eq!(count(&e.new_db, table), 4, "{table}");
            // Two of three go, three new rows take rowids 2, 3, 4.
            q(&e.old_db, &format!("DELETE FROM {table} WHERE rowid > 1"));
            for t in ["x", "y", "z"] {
                add(table, &e.old_db, t);
            }
            e.merge();
            e.merge();
            assert_eq!(count(&e.new_db, table), 7, "{table}");
            let c = Connection::open(&e.new_db).unwrap();
            let mut st = c
                .prepare(&format!("SELECT ts FROM {table} ORDER BY ts"))
                .unwrap();
            let got: Vec<String> = st
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(got, ["a", "b", "c", "d", "x", "y", "z"], "{table}");
        }
    }

    #[test]
    fn a_vacuum_that_renumbers_the_source_loses_and_repeats_nothing() {
        for table in ["read_events", "faults"] {
            let e = env(V190);
            for t in ["a", "b", "c", "d"] {
                add(table, &e.old_db, t);
            }
            e.merge();
            q(&e.old_db, &format!("DELETE FROM {table} WHERE rowid = 2"));
            q(&e.old_db, "VACUUM");
            e.merge();
            assert_eq!(count(&e.new_db, table), 4, "{table}");
            add(table, &e.old_db, "e");
            e.merge();
            assert_eq!(count(&e.new_db, table), 5, "{table}");
        }
    }

    #[test]
    fn genuinely_identical_rows_in_the_source_are_all_carried() {
        for table in ["read_events", "faults"] {
            let e = env(V190);
            add(table, &e.old_db, "t1");
            add(table, &e.old_db, "t1");
            e.merge();
            e.merge();
            assert_eq!(count(&e.new_db, table), 2, "{table}");
            // After a rewind the same rule holds: two carried, two present, none added;
            // a third identical row is a new row only if it has a new rowid past the mark.
            add(table, &e.old_db, "t1");
            e.merge();
            assert_eq!(count(&e.new_db, table), 3, "{table}");
            q(&e.old_db, &format!("DELETE FROM {table}"));
            add(table, &e.old_db, "t1");
            add(table, &e.old_db, "t1");
            add(table, &e.old_db, "t1");
            add(table, &e.old_db, "t9");
            e.merge();
            assert_eq!(count(&e.new_db, table), 4, "{table}: only t9 is new");
        }
    }

    #[test]
    fn rows_the_copy_carried_count_as_carried_when_the_source_restarts() {
        let e = env(V190);
        for t in ["a", "b", "c"] {
            add_read(&e.old_db, t, "/a", "builtin_read", None);
        }
        // The migration's copy put a and b in the new ledger.
        add_read(&e.new_db, "a", "/a", "builtin_read", None);
        add_read(&e.new_db, "b", "/a", "builtin_read", None);
        let mut m = marker();
        m.hwm.insert("read_events".into(), 2);
        merge_late(&e.new_db, &[&e.old_db], &m).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 3);
        q(&e.old_db, "DELETE FROM read_events");
        add_read(&e.old_db, "a", "/a", "builtin_read", None);
        add_read(&e.old_db, "d", "/a", "builtin_read", None);
        merge_late(&e.new_db, &[&e.old_db], &marker()).unwrap();
        assert_eq!(
            count(&e.new_db, "read_events"),
            4,
            "d is new; a is not again"
        );
    }

    #[test]
    fn the_normal_merge_keeps_nothing_per_row() {
        let e = env(V190);
        let bookkeeping = |e: &Env| {
            ["legacy_merge", "legacy_spool_intent"]
                .iter()
                .map(|t| count(&e.new_db, t))
                .collect::<Vec<_>>()
        };
        q(
            &e.old_db,
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10) \
             INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
             routed_via) SELECT 't' || i, 'Read', '/a', 1, 2, 1, 'builtin_read' FROM n",
        );
        e.merge();
        let small = bookkeeping(&e);
        q(
            &e.old_db,
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000) \
             INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
             routed_via) SELECT 'u' || i, 'Read', '/a', 1, 2, 1, 'builtin_read' FROM n",
        );
        e.merge();
        assert_eq!(count(&e.new_db, "read_events"), 5010);
        assert_eq!(
            bookkeeping(&e),
            small,
            "bookkeeping does not grow with the source"
        );
        assert_eq!(small, vec![2, 0]);
    }

    #[test]
    fn columns_only_the_new_ledger_has_take_their_defaults() {
        let e = env(
            "CREATE TABLE turns (message_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
                ts TEXT NOT NULL, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, \
                cache_read_input_tokens INTEGER NOT NULL, \
                cache_creation_input_tokens INTEGER NOT NULL); \
             CREATE TABLE read_events (ts TEXT NOT NULL, tool TEXT NOT NULL, path TEXT NOT NULL, \
                tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL, \
                saved_tokens INTEGER NOT NULL, routed_via TEXT NOT NULL); \
             CREATE TABLE faults (ts TEXT NOT NULL, kind TEXT NOT NULL, \
                variant TEXT NOT NULL DEFAULT '-');",
        );
        add_turn(&e.old_db, "m1");
        q(
            &e.old_db,
            "INSERT INTO read_events VALUES ('t', 'Read', '/a', 1, 2, 1, 'builtin_read')",
        );
        q(&e.old_db, "INSERT INTO faults (ts, kind) VALUES ('t', 'k')");
        let r = e.merge();
        assert_eq!(r.rows(), 3, "{r:?}");
        let c = Connection::open(&e.new_db).unwrap();
        let (sub, speed): (i64, Option<String>) = c
            .query_row("SELECT is_subagent, speed FROM turns", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((sub, speed), (0, None));
        for t in ["read_events", "faults"] {
            let ch: String = c
                .query_row(&format!("SELECT channel FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(ch, "unknown", "{t}");
        }
    }

    #[test]
    fn the_spool_is_appended_once_at_line_boundaries() {
        let e = env(V190);
        std::fs::write(e.src_spool(), "{\"a\":1}\n{\"a\":2}\n").unwrap();
        std::fs::write(e.dst_spool(), "{\"n\":0}\n").unwrap();
        let mut m = marker();
        m.spool_offset = 8; // the copy carried the first line
        let r = merge_late(&e.new_db, &[&e.old_db], &m).unwrap();
        assert_eq!(r.sources[0].spool_bytes, 8);
        assert_eq!(e.dst(), "{\"n\":0}\n{\"a\":2}\n");
        e.merge();
        // A half-written line waits; a finished one goes.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(e.src_spool())
            .unwrap();
        f.write_all(b"{\"a\":3").unwrap();
        e.merge();
        assert_eq!(e.dst(), "{\"n\":0}\n{\"a\":2}\n");
        f.write_all(b"}\n").unwrap();
        e.merge();
        e.merge();
        assert_eq!(e.dst(), "{\"n\":0}\n{\"a\":2}\n{\"a\":3}\n");
        assert_eq!(count(&e.new_db, "legacy_spool_intent"), 0);
    }

    #[test]
    fn identical_spool_lines_are_all_carried() {
        let e = env(V190);
        std::fs::write(e.src_spool(), "{\"a\":1}\n{\"a\":1}\n{\"a\":1}\n").unwrap();
        std::fs::write(e.dst_spool(), "{\"a\":1}\n").unwrap(); // N's own, equal to the tail
        e.merge();
        e.merge();
        assert_eq!(e.dst(), "{\"a\":1}\n".repeat(4));
    }

    #[test]
    fn a_crashed_spool_copy_is_finished_exactly() {
        // The intent says: bytes 8..24 of the source go at the end of a 5-byte destination.
        let lines = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
        for appended in [0usize, 9, 16] {
            // none of it, a torn part (a line and a bit), all of it
            let e = env(V190);
            std::fs::write(e.src_spool(), lines).unwrap();
            let chunk = &lines.as_bytes()[8..24];
            let mut dst = b"{n}\n\n".to_vec();
            dst.extend_from_slice(&chunk[..appended]);
            std::fs::write(e.dst_spool(), &dst).unwrap();
            // Progress and the pending intent, as a run killed mid-copy leaves them.
            q(
                &e.new_db,
                &format!(
                    "INSERT INTO legacy_merge VALUES ('{k}', 'spool', 8, NULL); \
                     INSERT INTO legacy_spool_intent VALUES ('{k}', 8, 24, 5)",
                    k = key_for(&e)
                ),
            );
            e.merge();
            e.merge();
            assert_eq!(
                e.dst(),
                format!("{{n}}\n\n{}", &lines[8..]),
                "appended={appended}"
            );
            assert_eq!(count(&e.new_db, "legacy_spool_intent"), 0);
        }
    }

    fn key_for(e: &Env) -> String {
        key(e).replace('\'', "''")
    }

    #[test]
    fn a_restarted_source_spool_is_read_from_its_start() {
        for second in [
            "{\"b\":1}\n",
            "{\"b\":1}\n{\"b\":2}\n{\"b\":3}\n{\"b\":4}\n",
        ] {
            // shorter than the offset, and longer than it
            let e = env(V190);
            std::fs::write(e.src_spool(), "{\"a\":1}\n{\"a\":2}\n").unwrap();
            e.merge();
            assert_eq!(e.dst(), "{\"a\":1}\n{\"a\":2}\n");
            std::fs::write(e.src_spool(), second).unwrap(); // drained, then written anew
            e.merge();
            assert_eq!(e.dst(), format!("{{\"a\":1}}\n{{\"a\":2}}\n{second}"));
        }
    }

    #[test]
    fn a_bad_source_is_skipped_and_reported_not_fatal() {
        let e = env(V190);
        seed(&e.old_db);
        let t = e.old_db.parent().unwrap();
        let junk = t.join("junk.db");
        std::fs::write(
            &junk,
            b"this is not sqlite, but it is longer than a header page",
        )
        .unwrap();
        let empty = old_ledger(&t.join("empty"), "CREATE TABLE unrelated (x)");
        let missing = t.join("nope.db");
        let r = merge_late(
            &e.new_db,
            &[&missing, &junk, &empty, &e.old_db, &e.new_db],
            &marker(),
        )
        .unwrap();
        let why = |i: usize| r.sources[i].skipped.clone().unwrap();
        assert!(why(0).starts_with("missing"), "{}", why(0));
        assert!(why(1).starts_with("not a ledger"), "{}", why(1));
        assert!(why(2).starts_with("not a ledger"), "{}", why(2));
        assert_eq!(r.sources[3].skipped, None);
        assert!(why(4).contains("same file"), "{}", why(4));
        assert_eq!(count(&e.new_db, "turns"), 2);
    }

    #[test]
    fn a_source_on_a_newer_schema_is_reported_and_merged_on_common_columns() {
        let e = env(V190);
        seed(&e.old_db);
        q(
            &e.old_db,
            "ALTER TABLE turns ADD COLUMN from_the_future TEXT",
        );
        let r = e.merge();
        assert!(
            r.sources[0]
                .notes
                .iter()
                .any(|n| n.contains("from_the_future")),
            "{r:?}"
        );
        assert_eq!(count(&e.new_db, "turns"), 2);
    }

    #[test]
    fn the_source_is_never_written() {
        let e = env(V190);
        seed(&e.old_db);
        let before = sha(&e.old_db);
        e.merge();
        assert_eq!(sha(&e.old_db), before);
        let names: Vec<_> = std::fs::read_dir(e.old_db.parent().unwrap())
            .unwrap()
            .map(|d| d.unwrap().file_name())
            .collect();
        assert_eq!(
            names.len(),
            1,
            "no journal or marker left behind: {names:?}"
        );
    }

    #[test]
    fn a_source_path_with_odd_characters_attaches() {
        let t = TempDir::new().unwrap();
        let new_dir = t.path().join("new");
        std::fs::create_dir_all(&new_dir).unwrap();
        let new_db = new_ledger(&new_dir);
        // `?` is the URI's query separator, but Windows does not allow it in a file name; there
        // `&` stands in, and the space, `#` and `%` still need escaping.
        let odd = if cfg!(windows) {
            "a b#c%d&e"
        } else {
            "a b#c%d?e"
        };
        let old = old_ledger(&t.path().join(odd), V190);
        add_turn(&old, "m");
        let r = merge_late(&new_db, &[&old], &marker()).unwrap();
        assert_eq!(r.rows(), 1, "{r:?}");
    }

    #[test]
    fn each_source_has_its_own_progress() {
        let e = env(V190);
        add_read(&e.old_db, "t1", "/a", "builtin_read", None);
        let other = old_ledger(&e.old_db.parent().unwrap().join("second"), V180);
        add_read(&other, "t1", "/a", "builtin_read", None);
        add_read(&other, "t1", "/a", "builtin_read", None);
        merge_late(&e.new_db, &[&e.old_db, &other], &marker()).unwrap();
        merge_late(&e.new_db, &[&e.old_db, &other], &marker()).unwrap();
        assert_eq!(count(&e.new_db, "read_events"), 3);
    }

    #[test]
    fn the_uri_for_a_path_is_read_only_and_escaped() {
        assert_eq!(
            ro_uri("/a b/c#d?e%f").unwrap(),
            "file:/a%20b/c%23d%3fe%25f?mode=ro"
        );
        assert_eq!(
            ro_uri(r"C:\Users\u\lumen.db").unwrap(),
            "file:/C:/Users/u/lumen.db?mode=ro"
        );
        assert_eq!(
            ro_uri(r"\\?\C:\Users\u\lumen.db").unwrap(),
            "file:/C:/Users/u/lumen.db?mode=ro"
        );
        for unc in [
            r"\\host\share\lumen.db",
            r"\\?\UNC\host\share\lumen.db",
            r"\\?\unc\h\s\x",
        ] {
            let e = ro_uri(unc).unwrap_err();
            assert!(e.contains("UNC"), "{unc}: {e}");
        }
    }

    #[test]
    fn the_tables_exist_through_every_opener() {
        // rusqlite (meter.rs), on a ledger from before the tables existed.
        let t = TempDir::new().unwrap();
        let p = old_ledger(t.path(), V180);
        drop(crate::meter::connect_db(&p).unwrap());
        for tbl in ["legacy_merge", "legacy_spool_intent"] {
            assert_eq!(count(&p, tbl), 0, "{tbl}");
        }
        // sqlx (daemon and GUI).
        let p2 = old_ledger(&t.path().join("x"), V190);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = crate::schema::open_pool(&format!("sqlite:{}", p2.display()))
                .await
                .unwrap();
            pool.close().await;
        });
        for tbl in ["legacy_merge", "legacy_spool_intent"] {
            assert_eq!(count(&p2, tbl), 0, "{tbl}");
        }
    }
}
