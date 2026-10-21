//! Faults merge into N by the maximum any one source holds.
//!
//! A fault reaches N from several places: the merge and the freeze of the backup, a manual
//! import's `faults` rows, and a spool (the backup's, a folder's, N's own). The routes overlap: an
//! old app drains its spool into its own table, a restored copy holds a spool the freeze took, a
//! copy of a spool-only folder comes after the folder. Bookkeeping per route cannot cover every
//! pair of them.
//!
//! The rule: each fault record has a digest over the core columns ([`CORE_FAULTS`]), the same for
//! a `faults` row and for a spool line. For each digest, N ends up holding the maximum count that
//! any single source holds: N's own records, the previous install (its rows, its spool and its
//! `.draining` batch are one source), and each imported copy (its rows and its spool). Every step
//! that adds faults applies the rule against N as it is at that moment and counts only its own
//! source: it adds `max(0, source - N)` records of that digest. The steps are the import, the
//! merge's row pass and the freeze's spool take. Nothing is anticipated (a source's unmerged rows
//! or unfrozen spool are not counted as N's), and since the maximum is commutative and
//! associative the result does not depend on the order of move, drain, merge, freeze and imports.
//!
//! Two genuinely identical faults within one source keep their count: the result is never below
//! any one source's count. `agent_id` is not in the digest, so that a spool line and the row an
//! older app drained it into have the same one.

use super::merge::{columns, key_expr};
use crate::faults::{self, FaultRecord};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// The columns of `faults` that every released schema has (1.5.0's table).
pub(super) const CORE_FAULTS: [&str; 9] = [
    "ts",
    "kind",
    "variant",
    "path",
    "lines",
    "detail",
    "session_id",
    "version",
    "channel",
];

/// How many records of each digest.
pub(super) type FaultCounts = BTreeMap<String, i64>;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A spool and its `.draining` batch: the records, in order (the batch first), and the bytes they
/// were read from. A spool that is not there has none.
pub(super) fn read_spool_files(live: &Path) -> Result<(Vec<FaultRecord>, Vec<u8>), String> {
    let mut bytes = Vec::new();
    let mut recs = Vec::new();
    for f in [faults::draining_path(live), live.to_path_buf()] {
        match fs::read(&f) {
            Ok(b) => {
                recs.extend(faults::parse_spool_all(&b));
                bytes.extend_from_slice(&b);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read {}: {e}", f.display())),
        }
    }
    Ok((recs, bytes))
}

/// The digest of each spool record, in order.
pub(super) fn record_digests(recs: &[FaultRecord]) -> Result<Vec<String>, String> {
    let mem = Connection::open_in_memory().map_err(|e| format!("spool digests: {e}"))?;
    let cols = CORE_FAULTS.join(", ");
    let run = || -> rusqlite::Result<Vec<String>> {
        mem.execute_batch(&format!("CREATE TABLE s ({cols})"))?;
        let mut ins = mem.prepare(&format!(
            "INSERT INTO s ({cols}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
        ))?;
        for r in recs {
            ins.execute(params![
                r.ts,
                r.kind,
                r.variant,
                r.path,
                r.lines,
                r.detail,
                r.session_id,
                r.version,
                r.channel
            ])?;
        }
        let mut st = mem.prepare(&format!(
            "SELECT {} FROM s ORDER BY rowid",
            key_expr("", &CORE_FAULTS)
        ))?;
        let keys = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys.iter().map(|k| hex(k.as_bytes())).collect())
    };
    run().map_err(|e| format!("spool digests: {e}"))
}

/// The rowid and digest of every row of `schema.faults`, in rowid order; `None` when the table is
/// not there or lacks a core column (a digest cannot be made).
pub(super) fn row_digests(
    conn: &Connection,
    schema: &str,
) -> Result<Option<Vec<(i64, String)>>, String> {
    let cols = columns(conn, schema, "faults").map_err(|e| format!("faults: {e}"))?;
    if CORE_FAULTS.iter().any(|c| !cols.iter().any(|x| x == c)) {
        return Ok(None);
    }
    let mut st = conn
        .prepare(&format!(
            "SELECT rowid, {} FROM {schema}.faults ORDER BY rowid",
            key_expr("", &CORE_FAULTS)
        ))
        .map_err(|e| format!("faults: {e}"))?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("faults: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("faults: {e}"))?;
    Ok(Some(
        rows.into_iter()
            .map(|(id, k)| (id, hex(k.as_bytes())))
            .collect(),
    ))
}

/// Count digests.
pub(super) fn tally<'a>(keys: impl IntoIterator<Item = &'a String>) -> FaultCounts {
    let mut out = FaultCounts::new();
    for k in keys {
        *out.entry(k.clone()).or_insert(0) += 1;
    }
    out
}

/// What N holds right now: its `faults` table (in `schema`) and its own spool that has not been
/// drained. Nothing of the backup or of another source is counted as N's: a step counts only
/// its own source against what N is at that moment.
pub(super) fn counts_in_n(
    conn: &Connection,
    schema: &str,
    n_dir: &Path,
) -> Result<FaultCounts, String> {
    let mut out = FaultCounts::new();
    if let Some(rows) = row_digests(conn, schema)? {
        out = tally(rows.iter().map(|(_, k)| k));
    }
    let (recs, _) = read_spool_files(&n_dir.join("faults.jsonl"))?;
    for k in record_digests(&recs)? {
        *out.entry(k).or_insert(0) += 1;
    }
    Ok(out)
}

/// Which of the candidates (digests, in the order they would be added) to add: one goes in while
/// the source's count of its digest is above N's, and each one taken counts as N's.
pub(super) fn admit_fresh(
    source: &FaultCounts,
    n: &FaultCounts,
    candidates: &[String],
) -> Vec<bool> {
    let mut room: BTreeMap<&str, i64> = BTreeMap::new();
    candidates
        .iter()
        .map(|k| {
            let r = room.entry(k.as_str()).or_insert_with(|| {
                let held = |m: &FaultCounts| m.get(k).copied().into_iter().sum::<i64>();
                held(source) - held(n)
            });
            if *r > 0 {
                *r -= 1;
                true
            } else {
                false
            }
        })
        .collect()
}
