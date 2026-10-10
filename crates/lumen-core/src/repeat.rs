//! Repeat reads (1.7.0): answering a second `smart_read` of the same file with less than a
//! second outline.
//!
//! A repeat is only cheap to answer if the model still HAS the first answer. Two things take
//! it away: the file changed (then say what changed), or the conversation was compacted (then
//! say everything again). Everything here exists to tell those apart without guessing, and
//! every doubt resolves to a full outline, never to a stub the model cannot use.
//!
//! * `read_state` remembers, per `(session, path)`, what was delivered: a content hash, the
//!   item signatures, and the transcript length at that moment.
//! * Compaction is detected from the transcript: a `compact_boundary` record written after
//!   that length. A transcript that cannot be found or read is "unverifiable", which is
//!   treated like a compaction.
//! * The flag is `LUMEN_REPEAT_READS` (off by default; `on`; `ab` splits by path hash).

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ranked::{Mode, path_hash};
use crate::structure::CodeItem;

pub const ROUTE_UNCHANGED: &str = "smart_read_unchanged";
pub const ROUTE_DELTA: &str = "smart_read_delta";

pub const ARM_TREATMENT: &str = "treatment";
pub const ARM_CONTROL: &str = "control";

/// Read the rollout mode from `LUMEN_REPEAT_READS`. Anything unrecognised is `Off`.
pub fn mode_from_env() -> Mode {
    parse_mode(&std::env::var("LUMEN_REPEAT_READS").unwrap_or_default())
}

pub fn parse_mode(v: &str) -> Mode {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Mode::On,
        "ab" => Mode::Ab,
        _ => Mode::Off,
    }
}

/// Whether this path gets the repeat-read path. The salt keeps this split independent of the
/// ranked-outline split: with the same hash, "ranked" and "repeat" would be the same files,
/// and neither experiment could be read without the other.
pub fn treated(mode: Mode, path: &str) -> bool {
    match mode {
        Mode::Off => false,
        Mode::On => true,
        Mode::Ab => !path_hash(format!("repeat\0{path}").as_bytes()).is_multiple_of(2),
    }
}

/// One outline item as the model saw it, with a hash of its text so an edit inside it is
/// visible even when its line range did not change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSig {
    pub k: String,
    pub n: Option<String>,
    pub s: usize,
    pub e: usize,
    pub h: u64,
}

/// What was delivered for a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivered {
    pub hash: String,
    pub lines: usize,
    /// Empty when no outline was delivered (the whole file was): nothing to diff against.
    pub items: Vec<ItemSig>,
    /// Transcript length when it was delivered. `None`: no transcript, so no way to know
    /// whether it was compacted away.
    pub tx_len: Option<u64>,
}

/// Content fingerprint: 16 hex chars of SHA-256.
pub fn fingerprint(src: &str) -> String {
    let d = Sha256::digest(src.as_bytes());
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn item_sigs(src: &str, items: &[CodeItem]) -> Vec<ItemSig> {
    items
        .iter()
        .map(|i| ItemSig {
            k: i.kind.clone(),
            n: i.name.clone(),
            s: i.start_line,
            e: i.end_line,
            h: path_hash(src.get(i.start_byte..i.end_byte).unwrap_or("").as_bytes()),
        })
        .collect()
}

// ── the decision ─────────────────────────────────────────────────────────────

/// What to do with a repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Deliver a full outline. The reason is what the ledger records.
    Fresh(&'static str),
    Unchanged,
    Delta(Delta),
}

impl Plan {
    pub fn label(&self) -> &'static str {
        match self {
            Plan::Fresh(r) => r,
            Plan::Unchanged => "unchanged",
            Plan::Delta(_) => "delta",
        }
    }
}

/// `compacted`: `Some(false)` only when the transcript was read and holds no boundary since
/// the delivery. Anything else — a boundary, or not knowing — is a fresh outline.
pub fn decide(
    prev: Option<&Delivered>,
    hash: &str,
    items: &[ItemSig],
    compacted: Option<bool>,
) -> Plan {
    let Some(p) = prev else {
        return Plan::Fresh("first");
    };
    match compacted {
        Some(true) => return Plan::Fresh("compacted"),
        None => return Plan::Fresh("unverifiable"),
        Some(false) => {}
    }
    if p.hash == hash {
        return Plan::Unchanged;
    }
    // A whole-file "item" or an empty baseline has nothing to diff.
    if p.items.is_empty() || items.is_empty() {
        return Plan::Fresh("not_diffable");
    }
    Plan::Delta(diff(&p.items, items))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delta {
    pub edited: Vec<ItemSig>,
    pub moved: Vec<(ItemSig, ItemSig)>,
    pub added: Vec<ItemSig>,
    pub removed: Vec<ItemSig>,
    /// `(old_start_line, offset)`: every item from that old line on kept its text and moved
    /// by `offset`. A single insertion moves everything below it, and listing each of those
    /// would make the delta as long as the outline it replaces.
    pub shifted_from: Option<(usize, i64)>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.edited.is_empty()
            && self.moved.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.shifted_from.is_none()
    }
}

/// Smallest run of identical shifts worth collapsing into one line.
const MIN_SHIFT_RUN: usize = 3;

/// Match named items by `(kind, name, nth occurrence)` and anonymous ones (imports) by
/// `(kind, text hash, nth occurrence)`: an inserted import must not turn every import below
/// it into an edit.
pub fn diff(old: &[ItemSig], new: &[ItemSig]) -> Delta {
    use std::collections::HashMap;
    type Key = (String, Option<String>, Option<u64>, usize);

    fn keys(items: &[ItemSig]) -> Vec<Key> {
        let mut seen: HashMap<(String, Option<String>, Option<u64>), usize> = HashMap::new();
        items
            .iter()
            .map(|i| {
                let base = (
                    i.k.clone(),
                    i.n.clone(),
                    if i.n.is_none() { Some(i.h) } else { None },
                );
                let nth = seen.entry(base.clone()).or_insert(0);
                let key = (base.0, base.1, base.2, *nth);
                *nth += 1;
                key
            })
            .collect()
    }

    let old_keys = keys(old);
    let new_keys = keys(new);
    let old_by_key: HashMap<&Key, usize> =
        old_keys.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut matched_old = vec![false; old.len()];

    let mut d = Delta::default();
    // Per new item: Some(offset) when matched, unedited and moved.
    let mut shift: Vec<Option<i64>> = vec![None; new.len()];
    for (ni, nk) in new_keys.iter().enumerate() {
        match old_by_key.get(nk) {
            None => d.added.push(new[ni].clone()),
            Some(&oi) => {
                matched_old[oi] = true;
                let (o, n) = (&old[oi], &new[ni]);
                if o.h != n.h {
                    d.edited.push(n.clone());
                } else if (o.s, o.e) != (n.s, n.e) {
                    shift[ni] = Some(n.s as i64 - o.s as i64);
                    d.moved.push((o.clone(), n.clone()));
                }
            }
        }
    }
    for (oi, m) in matched_old.iter().enumerate() {
        if !m {
            d.removed.push(old[oi].clone());
        }
    }

    // Collapse the trailing run of identical shifts into one statement.
    if let Some(Some(off)) = shift.last().copied() {
        let run = shift.iter().rev().take_while(|s| **s == Some(off)).count();
        if run >= MIN_SHIFT_RUN {
            let first_new = new.len() - run;
            let first_moved = d.moved.iter().position(|(_, n)| {
                n.s == new[first_new].s && n.k == new[first_new].k && n.n == new[first_new].n
            });
            if let Some(pos) = first_moved {
                d.shifted_from = Some((d.moved[pos].0.s, off));
                d.moved.truncate(pos);
            }
        }
    }
    d
}

// ── what the model is told ───────────────────────────────────────────────────

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// Deliberately tells the model how to recover: MCP carries no caller identity, so a
/// subagent sharing the parent's session can be answered "unchanged" about an outline it
/// never saw. One call with `refresh` gets it the full outline.
pub fn format_unchanged(path: &str, lines: usize, items: usize, hash: &str) -> String {
    format!(
        "# {path} — unchanged since your last smart_read this session ({lines} lines, {items} items, {})\n\
         # That outline is still exact; recall_file fetches bodies. Not in your context? smart_read(path, refresh=true).\n",
        short(hash)
    )
}

pub fn format_delta(
    path: &str,
    old: &Delivered,
    new_hash: &str,
    new_lines: usize,
    d: &Delta,
) -> String {
    let mut b = format!(
        "# {path} — changed since your last smart_read ({} → {}, {} → {} lines)\n\
         # Only differences are listed; every other item keeps the line range you have.\n",
        short(&old.hash),
        short(new_hash),
        old.lines,
        new_lines
    );
    let name = |i: &ItemSig| i.n.clone().unwrap_or_else(|| "(anonymous)".into());
    for i in &d.edited {
        b.push_str(&format!("~ {} {} L{}-{}\n", i.k, name(i), i.s, i.e));
    }
    for (_, i) in &d.moved {
        b.push_str(&format!("> {} {} L{}-{}\n", i.k, name(i), i.s, i.e));
    }
    for i in &d.added {
        b.push_str(&format!("+ {} {} L{}-{}\n", i.k, name(i), i.s, i.e));
    }
    for i in &d.removed {
        b.push_str(&format!("- {} {} (was L{}-{})\n", i.k, name(i), i.s, i.e));
    }
    if d.is_empty() {
        b.push_str(
            "# No outlined item changed: the edit is between items, in a comment, or whitespace.\n",
        );
    }
    if let Some((from, off)) = d.shifted_from {
        b.push_str(&format!(
            "# every unedited item from old L{from} on moved {off:+} lines\n"
        ));
    }
    b.push_str("# ~ edited  > moved  + added  - removed. smart_read(path, refresh=true) for the full outline.\n");
    b
}

// ── persistence ──────────────────────────────────────────────────────────────

/// What smart_read last delivered for `(session, path)`. Read-only and fail-open: a missing
/// ledger, table or row is simply "nothing delivered", which yields a fresh outline.
pub fn load(db: &Path, session: &str, path: &str) -> Option<Delivered> {
    let conn = crate::meter::open_read_only(db).ok()?;
    conn.query_row(
        "SELECT hash, lines, items, tx_len FROM read_state WHERE session_id = ?1 AND path = ?2",
        params![session, path],
        |r| {
            let items: String = r.get(2)?;
            // A row whose items cannot be read is no baseline at all: the read that follows
            // delivers a fresh outline and overwrites it.
            let items = serde_json::from_str(&items).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(Delivered {
                hash: r.get(0)?,
                lines: r.get::<_, i64>(1)? as usize,
                items,
                tx_len: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Record a delivery. Runs after the reply has gone out, so it may take its time.
pub fn save(db: &Path, session: &str, path: &str, d: &Delivered) -> rusqlite::Result<()> {
    let conn = crate::meter::connect_db(db)?;
    conn.execute(
        "INSERT OR REPLACE INTO read_state(session_id,path,hash,lines,items,tx_len) \
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            session,
            path,
            d.hash,
            d.lines as i64,
            serde_json::to_string(&d.items).unwrap_or_else(|_| "[]".into()),
            d.tx_len.map(|v| v as i64),
        ],
    )?;
    conn.execute(
        "DELETE FROM read_state WHERE ts < datetime('now','-14 days')",
        [],
    )?;
    Ok(())
}

// ── compaction ───────────────────────────────────────────────────────────────

/// `LUMEN_PROJECTS_DIR`, else `~/.claude/projects` — the daemon's resolution, so a test that
/// points one at a tempdir points both.
pub fn projects_dir() -> Option<PathBuf> {
    match std::env::var("LUMEN_PROJECTS_DIR") {
        Ok(d) if !d.trim().is_empty() => Some(PathBuf::from(d)),
        _ => dirs::home_dir().map(|h| h.join(".claude/projects")),
    }
}

/// `<projects>/<any project>/<session>.jsonl`. The id comes from the environment, so it is
/// checked before it becomes part of a path.
pub fn transcript_path(session: &str) -> Option<PathBuf> {
    if session.is_empty()
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let name = format!("{session}.jsonl");
    std::fs::read_dir(projects_dir()?)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

pub fn transcript_len(session: &str) -> Option<u64> {
    std::fs::metadata(transcript_path(session)?)
        .ok()
        .map(|m| m.len())
}

/// Whether the transcript gained a `compact_boundary` record after byte `since`.
///
/// `None` when that cannot be said: no transcript, unreadable, or shorter than `since`
/// (it was rewritten). Only lines that mention the marker are parsed, and only a
/// `{"type":"system","subtype":"compact_boundary"}` record counts, so the phrase appearing
/// in a file the model read does not.
pub fn compacted_since(session: &str, since: u64) -> Option<bool> {
    let mut f = std::fs::File::open(transcript_path(session)?).ok()?;
    if f.metadata().ok()?.len() < since {
        return None;
    }
    f.seek(SeekFrom::Start(since)).ok()?;
    for line in BufReader::new(f).split(b'\n') {
        let line = line.ok()?;
        if !line.windows(16).any(|w| w == b"compact_boundary") {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line)
            && v.get("type").and_then(|t| t.as_str()) == Some("system")
            && v.get("subtype").and_then(|t| t.as_str()) == Some("compact_boundary")
        {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(k: &str, n: Option<&str>, s: usize, e: usize, h: u64) -> ItemSig {
        ItemSig {
            k: k.into(),
            n: n.map(String::from),
            s,
            e,
            h,
        }
    }

    fn delivered(items: Vec<ItemSig>) -> Delivered {
        Delivered {
            hash: "aaaaaaaaaaaaaaaa".into(),
            lines: 100,
            items,
            tx_len: Some(10),
        }
    }

    #[test]
    fn mode_parsing_defaults_off_and_typos_do_not_enable() {
        assert_eq!(parse_mode(""), Mode::Off);
        for typo in ["of", "yes", "enable", "abb", "off"] {
            assert_eq!(parse_mode(typo), Mode::Off, "{typo:?}");
        }
        assert_eq!(parse_mode("on"), Mode::On);
        assert_eq!(parse_mode(" ON "), Mode::On);
        assert_eq!(parse_mode("ab"), Mode::Ab);
    }

    #[test]
    fn treatment_split_is_stable_balanced_and_independent_of_ranked() {
        assert!(!treated(Mode::Off, "a.rs"));
        assert!(treated(Mode::On, "a.rs"));
        let paths: Vec<String> = (0..4000)
            .map(|i| format!("src/module_{i}/file_{i}.rs"))
            .collect();
        let t = paths.iter().filter(|p| treated(Mode::Ab, p)).count();
        assert!((1800..2200).contains(&t), "split {t}/4000");
        for p in paths.iter().take(50) {
            assert_eq!(treated(Mode::Ab, p), treated(Mode::Ab, p));
        }
        let same = paths
            .iter()
            .filter(|p| {
                treated(Mode::Ab, p)
                    == (crate::ranked::arm_for(Mode::Ab, p) == crate::ranked::Arm::Ranked)
            })
            .count();
        assert!(
            (1800..2200).contains(&same),
            "correlated with ranked split: {same}/4000"
        );
    }

    #[test]
    fn decide_never_stubs_without_proof_the_outline_survived() {
        let items = vec![it("function", Some("a"), 1, 3, 1)];
        let p = delivered(items.clone());
        assert_eq!(decide(None, "x", &items, Some(false)), Plan::Fresh("first"));
        assert_eq!(
            decide(Some(&p), &p.hash, &items, Some(true)),
            Plan::Fresh("compacted")
        );
        assert_eq!(
            decide(Some(&p), &p.hash, &items, None),
            Plan::Fresh("unverifiable")
        );
        assert_eq!(
            decide(Some(&p), &p.hash, &items, Some(false)),
            Plan::Unchanged
        );
    }

    #[test]
    fn decide_without_an_outline_to_diff_goes_fresh() {
        let p = delivered(vec![]);
        let items = vec![it("function", Some("a"), 1, 3, 1)];
        assert_eq!(
            decide(Some(&p), "other", &items, Some(false)),
            Plan::Fresh("not_diffable")
        );
    }

    #[test]
    fn diff_classifies_edit_move_add_remove() {
        let old = vec![
            it("function", Some("a"), 1, 3, 10),
            it("function", Some("b"), 5, 9, 20),
            it("function", Some("gone"), 11, 12, 30),
            it("function", Some("c"), 14, 20, 40),
        ];
        let new = vec![
            it("function", Some("a"), 1, 3, 10),
            it("function", Some("b"), 5, 10, 21),
            it("function", Some("fresh"), 12, 14, 50),
            it("function", Some("c"), 16, 22, 40),
        ];
        let d = diff(&old, &new);
        assert_eq!(d.edited.len(), 1);
        assert_eq!(d.edited[0].n.as_deref(), Some("b"));
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.moved.len(), 1, "short run is listed, not collapsed");
        assert_eq!(d.moved[0].1.n.as_deref(), Some("c"));
        assert_eq!(d.shifted_from, None);
    }

    #[test]
    fn diff_collapses_a_trailing_shift_into_one_line() {
        let old: Vec<ItemSig> = (0..8)
            .map(|i| {
                it(
                    "function",
                    Some(&format!("f{i}")),
                    i * 10 + 1,
                    i * 10 + 5,
                    i as u64,
                )
            })
            .collect();
        let mut new = old.clone();
        for n in new.iter_mut().skip(3) {
            n.s += 4;
            n.e += 4;
        }
        new[2].e += 4;
        new[2].h = 99;
        let d = diff(&old, &new);
        assert_eq!(d.edited.len(), 1);
        assert!(d.moved.is_empty());
        assert_eq!(d.shifted_from, Some((31, 4)));
    }

    #[test]
    fn inserting_an_import_does_not_edit_the_imports_below_it() {
        let old = vec![
            it("import", None, 1, 1, 1),
            it("import", None, 2, 2, 2),
            it("function", Some("f"), 4, 9, 3),
        ];
        let new = vec![
            it("import", None, 1, 1, 1),
            it("import", None, 2, 2, 7),
            it("import", None, 3, 3, 2),
            it("function", Some("f"), 5, 10, 3),
        ];
        let d = diff(&old, &new);
        assert!(d.edited.is_empty(), "{d:?}");
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].h, 7);
    }

    #[test]
    fn identical_items_give_an_empty_delta() {
        let a = vec![it("function", Some("a"), 1, 3, 1)];
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn fingerprint_changes_with_one_byte_and_is_stable() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("").len(), 16);
    }

    #[test]
    fn state_round_trips_and_missing_state_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("l.db");
        assert!(load(&db, "s", "/a.rs").is_none(), "no ledger yet");
        let d = delivered(vec![it("function", Some("a"), 1, 3, 1)]);
        save(&db, "s", "/a.rs", &d).unwrap();
        assert_eq!(load(&db, "s", "/a.rs"), Some(d.clone()));
        assert!(
            load(&db, "other", "/a.rs").is_none(),
            "another session has its own state"
        );
        let mut d2 = d;
        d2.hash = "bbbbbbbbbbbbbbbb".into();
        save(&db, "s", "/a.rs", &d2).unwrap();
        assert_eq!(load(&db, "s", "/a.rs").unwrap().hash, "bbbbbbbbbbbbbbbb");
    }

    #[test]
    fn stale_state_is_pruned_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("l.db");
        let d = delivered(vec![]);
        save(&db, "old", "/a.rs", &d).unwrap();
        let conn = crate::meter::connect_db(&db).unwrap();
        conn.execute("UPDATE read_state SET ts = datetime('now','-15 days')", [])
            .unwrap();
        drop(conn);
        save(&db, "new", "/b.rs", &d).unwrap();
        assert!(load(&db, "old", "/a.rs").is_none());
        assert!(load(&db, "new", "/b.rs").is_some());
    }

    #[test]
    fn transcript_path_rejects_traversal() {
        for bad in ["", "../x", "a/b", "a\\b", "a b"] {
            assert!(transcript_path(bad).is_none(), "{bad:?}");
        }
    }

    /// The transcript-reading tests share the process environment, so they run in one test.
    #[test]
    fn compaction_is_read_from_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("-some-project");
        std::fs::create_dir_all(&proj).unwrap();
        let tx = proj.join("sess-1.jsonl");
        // SAFETY: the only test in this crate that touches this variable.
        unsafe { std::env::set_var("LUMEN_PROJECTS_DIR", dir.path()) };

        assert_eq!(
            compacted_since("sess-1", 0),
            None,
            "no transcript: unverifiable"
        );
        assert_eq!(transcript_len("sess-1"), None);

        let user = r#"{"type":"user","message":{"content":"hi"}}"#;
        std::fs::write(&tx, format!("{user}\n")).unwrap();
        let at = transcript_len("sess-1").unwrap();
        assert_eq!(compacted_since("sess-1", at), Some(false));

        // The phrase inside a tool result (a file the model read) is not a boundary.
        let quoted = r#"{"type":"user","message":{"content":"grep compact_boundary"}}"#;
        std::fs::write(&tx, format!("{user}\n{quoted}\n")).unwrap();
        assert_eq!(compacted_since("sess-1", at), Some(false));

        let boundary =
            r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted"}"#;
        std::fs::write(&tx, format!("{user}\n{quoted}\n{boundary}\n")).unwrap();
        assert_eq!(compacted_since("sess-1", at), Some(true));

        // A boundary from before the delivery does not count against it.
        let after = transcript_len("sess-1").unwrap();
        assert_eq!(compacted_since("sess-1", after), Some(false));

        // Shorter than the recorded offset: the file was rewritten.
        assert_eq!(compacted_since("sess-1", after + 1000), None);
    }
}
