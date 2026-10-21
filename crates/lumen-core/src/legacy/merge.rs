//! What the merge from a backup ([`super::freeze`]) and the manual import ([`super::import`])
//! share: the table lists, identifier quoting, the read-only `ATTACH` URI, column lists, the
//! per-source progress rows in `legacy_merge`, and the row digest.
//!
//! The ledger has no unique key on `read_events` or `faults` (two sessions, two subagents or two
//! ranged reads of one file can produce rows that agree on every column but their rowid), so
//! positional tables are carried by rowid and never matched by content here.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest as _, Sha256};

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

pub(super) fn quote(ident: &str) -> String {
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
pub(super) fn progress(
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

pub(super) fn set_progress(
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

/// SQL for one string that is equal for two rows exactly when every listed column is equal
/// (NULL equal to NULL): each value quoted, so no value can be mistaken for the separator.
pub(super) fn key_expr(alias: &str, cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("quote({alias}{})", quote(c)))
        .collect::<Vec<_>>()
        .join(" || ',' || ")
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
