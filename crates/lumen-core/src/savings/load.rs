//! Reading the engine's rows out of the ledger.
//!
//! The SQL lives here, once, and both loaders run it: this one (rusqlite, for the CLI, the MCP
//! server and the benchmark harness) and lumen-stats' (sqlx, for the app). Same statements, same
//! order, same rows, so the two summaries are the same bytes; a test in lumen-cli holds them to it.
//!
//! The calls and the turns come back packed, as one JSON array per statement or per session,
//! and are unpacked here by both loaders. A ledger ten times the size of a heavy user's holds a
//! million turns, and fetching a million rows column by column cost the sqlx loader four seconds
//! and the rusqlite one most of one; a few thousand packed rows cost a fraction of either. A
//! turn's model, speed and region are named once ([`super::TurnClass`]) and its time is read by
//! SQLite (`unixepoch`), so the unpacking allocates almost nothing per turn.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use super::{
    BlockedReadRow, BoundaryRow, CallRow, Clock, Ledger, LoadedTurn, RecordedBlockRow, ScannedRow,
    SessionTurns, TurnClass, ValveReadRow,
};

/// The Lumen calls as one JSON array of arrays (see [`parse_calls`], which puts them in the
/// order they were written). The route list is [`super::CALL_ROUTES`] (a test holds the two
/// together).
pub const CALLS_SQL: &str = "SELECT json_group_array(json_array(rowid, ts, tool, path, \
     routed_via, full_tokens, tokens_returned, saved_tokens, session_id, agent_id, req_key, \
     token_source, repeat_arm, repeat_plan)) \
     FROM read_events WHERE routed_via IN ('smart_read', 'recall_file', 'compress_logs', \
     'smart_read_full', 'would_inflate', 'ranked_outline', 'ranked_no_query', 'ranked_no_defs', \
     'ranked_not_worth_it', 'ranked_too_slow', 'smart_read_unchanged', 'smart_read_delta', \
     'recall_file_unchanged', 'recall_file_delta')";

/// The turns of the sessions in `?1`, a JSON array: only those a call or a blocked Read names,
/// since no other turn prices anything. One row per session, its turns packed as a JSON array of
/// arrays (see [`parse_turns`]), in no order: the engine orders each conversation by time and
/// rowid. The transcript path comes back only for a subagent's turn, which names the subagent:
/// the daemon marks those as it reads them, and the backfill every opener runs marks older rows,
/// under either separator.
pub const TURNS_SQL: &str = "SELECT session_id, json_group_array(json_array(rowid, \
     CAST(ROUND(unixepoch(ts, 'subsec') * 1000) AS INTEGER), model, speed, inference_geo, \
     input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, cache_5m, \
     cache_1h, COALESCE(is_subagent, 0), \
     CASE WHEN COALESCE(is_subagent, 0) = 1 THEN source_file END)) \
     FROM turns WHERE session_id IN (SELECT value FROM json_each(?1)) \
     GROUP BY session_id ORDER BY session_id";

/// Compactions (a subagent's included) and rewinds of the sessions in `?1`.
pub const BOUNDARIES_SQL: &str = "SELECT session_id, kind, ts_ms, source_file FROM session_events \
     WHERE kind IN ('compaction', 'rewind', 'subagent_compaction') \
       AND session_id IN (SELECT value FROM json_each(?1)) \
     ORDER BY session_id, ts_ms, kind, uuid";

/// When the daemon began recording events, as the ledger shows it: the earliest event of a main
/// transcript, and the earliest subagent compaction (see [`super::Ledger::events_from`]); and when
/// the meter began recording agents, the earliest row that names one
/// ([`super::Ledger::meter_agents_from`]).
pub const COVERAGE_SQL: &str = "SELECT \
     (SELECT MIN(ts_ms) FROM session_events \
        WHERE kind IN ('compaction', 'rewind', 'resync') \
          AND NOT (COALESCE(source_file, '') LIKE '%/subagents/%' \
                   OR COALESCE(source_file, '') LIKE '%\\subagents\\%')), \
     (SELECT MIN(ts_ms) FROM session_events WHERE kind = 'subagent_compaction'), \
     (SELECT CAST(ROUND(unixepoch(MIN(ts), 'subsec') * 1000) AS INTEGER) FROM read_events \
        WHERE agent_id IS NOT NULL)";

/// The meter's built-in Reads in the sessions `?1` and of the paths `?2` of blocked Reads whose
/// fault names no agent (see [`valve_keys_json`]), packed as one JSON array of arrays (see
/// [`parse_valve_reads`]): fetched row by row, a ledger's worth of them cost the sqlx loader a
/// third of its time.
pub const VALVE_READS_SQL: &str = "SELECT json_group_array(json_array(session_id, path, \
     CAST(ROUND(unixepoch(ts, 'subsec') * 1000) AS INTEGER), agent_id)) FROM read_events \
     WHERE routed_via = 'builtin_read' AND session_id IN (SELECT value FROM json_each(?1)) \
       AND path IN (SELECT value FROM json_each(?2))";

/// Main-agent turns of any session at or after `?1`, an ISO time with no zone letter (see
/// [`recent_since`]). Compared as text so `idx_turns_ts` serves it: every turn's time is the
/// transcript's ISO UTC, which sorts as text in time order.
pub const RECENT_TURNS_SQL: &str = "SELECT COUNT(*) FROM turns \
     WHERE ts >= ?1 AND COALESCE(is_subagent, 0) = 0";

/// Blocked Reads the escape valve let through, as the fault table holds them.
pub const BLOCKED_SQL: &str = "SELECT ts, session_id, path, detail, agent_id, version \
     FROM faults WHERE kind = 'hook_fail_open' AND variant = 'retry_escape_valve' ORDER BY rowid";

/// The blocked Reads the transcripts record, as one JSON array of arrays (see
/// [`parse_recorded_blocks`], which puts them in the order they were written). All of them: each
/// is a line of its own, a session with no Lumen call included.
pub const RECORDED_BLOCKS_SQL: &str = "SELECT json_group_array(json_array(rowid, session_id, \
     agent_id, path, ts_ms, kind)) FROM intercept_blocks";

/// The transcripts the daemon read for blocks, for the sessions in `?1`: each file, the session
/// and the agent it read, and through which of that session's turns (see [`parse_scanned`]).
pub const SCANNED_SQL: &str = "SELECT json_group_array(json_array(source_file, session_id, \
     agent_id, through_ms)) FROM transcript_scans \
     WHERE session_id IN (SELECT value FROM json_each(?1))";

/// What changes when anything the summary reads changes: the newest row of each table it reads,
/// and how far the daemon has read the transcripts for blocks (a row it moves on, not adds). A
/// refresh that finds the same six can keep the summary it has, unless the clock has moved it (a
/// new week, seven days without a call).
pub const FINGERPRINT_SQL: &str = "SELECT (SELECT MAX(rowid) FROM read_events), \
     (SELECT MAX(rowid) FROM turns), (SELECT MAX(rowid) FROM session_events), \
     (SELECT MAX(rowid) FROM faults), (SELECT MAX(rowid) FROM intercept_blocks), \
     (SELECT MAX(through_ms) FROM transcript_scans)";

/// For `?1` (unix seconds): the unix seconds of Monday 00:00 of its ISO week in local time, and
/// that Monday's local date. SQLite's `localtime` is the machine's zone, so the app and the CLI,
/// running as the same user, agree.
pub const WEEK_SQL: &str = "SELECT \
     CAST(strftime('%s', date(?1, 'unixepoch', 'localtime', \
       '-' || ((CAST(strftime('%w', ?1, 'unixepoch', 'localtime') AS INTEGER) + 6) % 7) \
       || ' days'), 'utc') AS INTEGER), \
     date(?1, 'unixepoch', 'localtime', \
       '-' || ((CAST(strftime('%w', ?1, 'unixepoch', 'localtime') AS INTEGER) + 6) % 7) \
       || ' days')";

/// The blocked Reads still in the fault spool, which `lumen faults` has not drained into the
/// table yet, and what went wrong reading it. Read, never drained: a summary writes nothing.
pub fn spooled_blocked_reads(spool: Option<&Path>) -> (Vec<BlockedReadRow>, Option<String>) {
    let Some(spool) = spool else {
        return (Vec::new(), None);
    };
    match crate::faults::peek_spool_at(spool) {
        Ok(recs) => (
            recs.into_iter()
                .filter(|r| r.kind == "hook_fail_open" && r.variant == "retry_escape_valve")
                .map(|r| BlockedReadRow {
                    ts: r.ts,
                    session_id: r.session_id,
                    path: r.path,
                    detail: r.detail,
                    agent_id: r.agent_id,
                    version: r.version,
                })
                .collect(),
            None,
        ),
        Err(e) => (
            Vec::new(),
            Some(format!(
                "the fault spool could not be read ({e}), so blocked Reads still in it are missing"
            )),
        ),
    }
}

/// The sessions and the paths of blocked Reads whose fault names no agent, as the two JSON arrays
/// [`VALVE_READS_SQL`] binds.
pub fn valve_keys_json(blocked: &[BlockedReadRow]) -> (String, String) {
    let unnamed = || blocked.iter().filter(|b| b.agent_id.is_none());
    let list = |mut v: Vec<&str>| {
        v.sort_unstable();
        v.dedup();
        serde_json::Value::from(v).to_string()
    };
    (
        list(unnamed().filter_map(|b| b.session_id.as_deref()).collect()),
        list(unnamed().filter_map(|b| b.path.as_deref()).collect()),
    )
}

/// [`VALVE_READS_SQL`]'s packed rows, in no particular order (the engine takes the nearest to
/// each fault). `None` is no row.
pub fn parse_valve_reads(packed: Option<&str>) -> Result<Vec<ValveReadRow>, String> {
    let Some(packed) = packed else {
        return Ok(Vec::new());
    };
    let rows: Vec<(String, String, Option<i64>, Option<String>)> = serde_json::from_str(packed)
        .map_err(|e| format!("the meter's Reads could not be unpacked: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(session_id, path, ts_ms, agent_id)| ValveReadRow {
            session_id,
            path,
            ts_ms,
            agent_id,
        })
        .collect())
}

/// [`RECORDED_BLOCKS_SQL`]'s packed rows, in the order they were written. `None` is no row.
pub fn parse_recorded_blocks(packed: Option<&str>) -> Result<Vec<RecordedBlockRow>, String> {
    let Some(packed) = packed else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(i64, String, Option<String>, String, i64, String)> =
        serde_json::from_str(packed)
            .map_err(|e| format!("the blocks the transcripts record did not unpack: {e}"))?;
    rows.sort_by_key(|r| r.0);
    Ok(rows
        .into_iter()
        .map(
            |(_, session_id, agent_id, path, ts_ms, kind)| RecordedBlockRow {
                session_id,
                agent_id,
                path,
                ts_ms,
                kind,
            },
        )
        .collect())
}

/// [`SCANNED_SQL`]'s packed rows, ordered so both loaders agree. `None` is no row.
pub fn parse_scanned(packed: Option<&str>) -> Result<Vec<ScannedRow>, String> {
    let Some(packed) = packed else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(String, String, Option<String>, i64)> = serde_json::from_str(packed)
        .map_err(|e| format!("the transcripts read for blocks did not unpack: {e}"))?;
    rows.sort();
    Ok(rows
        .into_iter()
        .map(
            |(source_file, session_id, agent_id, through_ms)| ScannedRow {
                source_file,
                session_id,
                agent_id,
                through_ms,
            },
        )
        .collect())
}

/// Every session a call, a blocked Read's fault or a block a transcript records names, sorted.
fn session_ids<'a>(
    calls: &'a [CallRow],
    blocked: &'a [BlockedReadRow],
    recorded: &'a [RecordedBlockRow],
) -> Vec<&'a str> {
    let mut ids: Vec<&str> = calls
        .iter()
        .filter_map(|c| c.session_id.as_deref())
        .chain(blocked.iter().filter_map(|b| b.session_id.as_deref()))
        .chain(recorded.iter().map(|b| b.session_id.as_str()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The sessions whose turns the engine needs, as the JSON array [`TURNS_SQL`] binds.
pub fn sessions_json(
    calls: &[CallRow],
    blocked: &[BlockedReadRow],
    recorded: &[RecordedBlockRow],
) -> String {
    serde_json::Value::from(session_ids(calls, blocked, recorded)).to_string()
}

/// How many statements the turns are read in, side by side: a ledger of a million turns reads in
/// a quarter of the time on four connections.
pub const TURN_CHUNKS: usize = 4;

/// [`sessions_json`] cut into at most [`TURN_CHUNKS`] arrays of consecutive sessions, each read
/// by its own [`TURNS_SQL`] on its own connection. Read in this order, the chunks give the
/// sessions in the order one statement would.
pub fn session_chunks(
    calls: &[CallRow],
    blocked: &[BlockedReadRow],
    recorded: &[RecordedBlockRow],
) -> Vec<String> {
    let ids = session_ids(calls, blocked, recorded);
    if ids.is_empty() {
        return Vec::new();
    }
    let per = ids.len().div_ceil(TURN_CHUNKS);
    ids.chunks(per)
        .map(|c| serde_json::Value::from(c.to_vec()).to_string())
        .collect()
}

/// The turns of every chunk, each unpacked on a thread of its own, then put together in chunk
/// order. Each chunk names its classes as it meets them, and they are renumbered here chunk by
/// chunk, which gives the numbering one pass in order would: both loaders agree.
pub fn parse_chunks(
    chunks: Vec<Vec<(String, String)>>,
) -> Result<(Vec<SessionTurns>, Vec<TurnClass>), String> {
    type Parsed = Result<(Vec<SessionTurns>, Vec<TurnClass>), String>;
    let parsed: Vec<Parsed> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let mut classes = Classes::default();
                    let mut out = Vec::with_capacity(chunk.len());
                    for (id, text) in chunk {
                        out.push(parse_turns(id, &text, &mut classes)?);
                    }
                    Ok((out, classes.list))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| match h.join() {
                Ok(r) => r,
                Err(_) => Err("a thread unpacking turns panicked".to_string()),
            })
            .collect()
    });
    let mut classes = Classes::default();
    let mut sessions = Vec::new();
    for chunk in parsed {
        let (mut turns, local) = chunk?;
        let renumber: Vec<u32> = local
            .iter()
            .map(|c| {
                classes.of(
                    c.model.as_deref(),
                    c.speed.as_deref(),
                    c.inference_geo.as_deref(),
                )
            })
            .collect();
        for s in &mut turns {
            for t in &mut s.turns {
                t.class = renumber[t.class as usize];
            }
        }
        sessions.extend(turns);
    }
    Ok((sessions, classes.list))
}

/// "Seven days before `now_ms`", as [`RECENT_TURNS_SQL`] binds it: `YYYY-MM-DDTHH:MM:SS`, which
/// every turn of that second or later sorts at or after, whatever fraction and zone letter
/// follow.
pub fn recent_since(now_ms: i64) -> String {
    let iso = super::iso_ms(now_ms - super::RECENT_DAYS * 24 * 60 * 60 * 1000);
    iso.trim_end_matches('Z').to_string()
}

/// One call, as [`CALLS_SQL`] packs it.
type PackedCall = (
    i64,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The calls [`CALLS_SQL`] packed, in the order they were written (rowid): sorted here, since an
/// aggregate's order is SQLite's to choose and sorting it there cost more. NULL (no calls) is
/// none.
pub fn parse_calls(packed: Option<&str>) -> Result<Vec<CallRow>, String> {
    let Some(packed) = packed else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<PackedCall> =
        serde_json::from_str(packed).map_err(|e| format!("the calls did not unpack: {e}"))?;
    rows.sort_by_key(|r| r.0);
    Ok(rows
        .into_iter()
        .map(|r| CallRow {
            rowid: r.0,
            ts: r.1,
            tool: r.2,
            path: r.3,
            routed_via: r.4,
            full_tokens: r.5,
            returned_tokens: r.6,
            saved_tokens: r.7,
            session_id: r.8,
            agent_id: r.9,
            req_key: r.10,
            token_source: r.11,
            repeat_arm: r.12,
            repeat_plan: r.13,
        })
        .collect())
}

/// Names each [`TurnClass`] once, in the order first met. Both loaders meet them in the same
/// order (sessions by id, as [`TURNS_SQL`] returns them), so the indexes agree.
#[derive(Default)]
pub struct Classes {
    index: HashMap<TurnClass, u32>,
    pub list: Vec<TurnClass>,
}

impl Classes {
    fn of(&mut self, model: Option<&str>, speed: Option<&str>, geo: Option<&str>) -> u32 {
        let class = TurnClass {
            model: model.map(str::to_string),
            speed: speed.map(str::to_string),
            inference_geo: geo.map(str::to_string),
        };
        if let Some(&i) = self.index.get(&class) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(class.clone());
        self.index.insert(class, i);
        i
    }
}

/// A cursor over the JSON [`TURNS_SQL`] writes: an array of arrays of integers, strings and
/// nulls, which SQLite writes with no whitespace. Read by hand because serde_json's general
/// parser was most of the load at a million turns; a string with an escape in it, which no model
/// name or path written by Claude Code holds, still goes through serde_json.
struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.peek() == Some(b) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", b as char, self.at))
        }
    }

    /// `,` between values, or the closing bracket (left in place).
    fn comma(&mut self) -> Result<(), String> {
        match self.peek() {
            Some(b',') => {
                self.at += 1;
                Ok(())
            }
            Some(b']') => Ok(()),
            _ => Err(format!("expected , or ] at byte {}", self.at)),
        }
    }

    fn null(&mut self) -> bool {
        if self.text[self.at..].starts_with("null") {
            self.at += 4;
            true
        } else {
            false
        }
    }

    fn int(&mut self) -> Result<i64, String> {
        let rest = &self.text[self.at..];
        let n = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'-')
            .count();
        let v = rest[..n]
            .parse()
            .map_err(|_| format!("not a whole number at byte {}", self.at))?;
        self.at += n;
        self.comma()?;
        Ok(v)
    }

    fn int_or_null(&mut self) -> Result<Option<i64>, String> {
        if self.null() {
            self.comma()?;
            return Ok(None);
        }
        self.int().map(Some)
    }

    fn string_or_null(&mut self) -> Result<Option<Cow<'a, str>>, String> {
        if self.null() {
            self.comma()?;
            return Ok(None);
        }
        let start = self.at;
        self.expect(b'"')?;
        let bytes = self.text.as_bytes();
        let mut escaped = false;
        let mut i = self.at;
        loop {
            match bytes.get(i) {
                Some(b'"') => break,
                Some(b'\\') => {
                    escaped = true;
                    i += 2;
                }
                Some(_) => i += 1,
                None => return Err(format!("an unterminated string at byte {start}")),
            }
        }
        let raw = &self.text[start..=i];
        self.at = i + 1;
        let value = if escaped {
            Cow::Owned(serde_json::from_str::<String>(raw).map_err(|e| e.to_string())?)
        } else {
            Cow::Borrowed(&raw[1..raw.len() - 1])
        };
        self.comma()?;
        Ok(Some(value))
    }
}

/// A turn's model, speed and region, as unpacked.
type Named<'a> = (
    Option<Cow<'a, str>>,
    Option<Cow<'a, str>>,
    Option<Cow<'a, str>>,
);

/// One session's turns, as [`TURNS_SQL`] packed them.
pub fn parse_turns(
    session_id: String,
    packed: &str,
    classes: &mut Classes,
) -> Result<SessionTurns, String> {
    let bad = |e: String| format!("the turns of {session_id} did not unpack: {e}");
    let mut c = Cursor {
        text: packed,
        at: 0,
    };
    let mut turns = Vec::new();
    // Consecutive turns mostly share a class: compare with the last one rather than hash each.
    let mut last: Option<(Named<'_>, u32)> = None;
    c.expect(b'[').map_err(bad)?;
    while c.peek() == Some(b'[') {
        c.at += 1;
        let id = c.int().map_err(bad)?;
        let ts_ms = c.int_or_null().map_err(bad)?;
        let model = c.string_or_null().map_err(bad)?;
        let speed = c.string_or_null().map_err(bad)?;
        let geo = c.string_or_null().map_err(bad)?;
        let input = c.int().map_err(bad)?;
        let output = c.int().map_err(bad)?;
        let cache_read = c.int().map_err(bad)?;
        let cache_write = c.int().map_err(bad)?;
        let cache_5m = c.int_or_null().map_err(bad)?;
        let cache_1h = c.int_or_null().map_err(bad)?;
        let is_subagent = c.int().map_err(bad)? != 0;
        let source_file = c.string_or_null().map_err(bad)?.map(Cow::into_owned);
        c.expect(b']').map_err(bad)?;
        c.comma().map_err(bad)?;
        let class = match &last {
            Some(((m, s, g), i)) if *m == model && *s == speed && *g == geo => *i,
            _ => {
                let i = classes.of(model.as_deref(), speed.as_deref(), geo.as_deref());
                last = Some(((model, speed, geo), i));
                i
            }
        };
        turns.push(LoadedTurn {
            id,
            ts_ms,
            class,
            input,
            output,
            cache_read,
            cache_write,
            cache_5m,
            cache_1h,
            is_subagent,
            source_file,
        });
    }
    c.expect(b']').map_err(bad)?;
    if c.at != packed.len() {
        return Err(bad(format!("{} bytes left over", packed.len() - c.at)));
    }
    Ok(SessionTurns { session_id, turns })
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

/// The ledger and the clock, through rusqlite. `spool` is the fault spool to read pending
/// blocked Reads from (`None` reads only the table).
pub fn load(
    conn: &rusqlite::Connection,
    now_ms: i64,
    spool: Option<&Path>,
) -> Result<(Ledger, Clock), String> {
    let now_s = now_ms.div_euclid(1000);
    let (week_start_s, week_start): (i64, String) = conn
        .query_row(WEEK_SQL, [now_s], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sql)?;

    let packed: Option<String> = conn.query_row(CALLS_SQL, [], |r| r.get(0)).map_err(sql)?;
    let calls = parse_calls(packed.as_deref())?;

    let mut stmt = conn.prepare(BLOCKED_SQL).map_err(sql)?;
    let mut blocked_reads = stmt
        .query_map([], |r| {
            Ok(BlockedReadRow {
                ts: r.get(0)?,
                session_id: r.get(1)?,
                path: r.get(2)?,
                detail: r.get(3)?,
                agent_id: r.get(4)?,
                version: r.get(5)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let (spooled, spool_gap) = spooled_blocked_reads(spool);
    blocked_reads.extend(spooled);
    let packed: Option<String> = conn
        .query_row(RECORDED_BLOCKS_SQL, [], |r| r.get(0))
        .map_err(sql)?;
    let recorded_blocks = parse_recorded_blocks(packed.as_deref())?;

    let sessions = sessions_json(&calls, &blocked_reads, &recorded_blocks);
    let chunks = session_chunks(&calls, &blocked_reads, &recorded_blocks);
    let read =
        |conn: &rusqlite::Connection, chunk: &str| -> Result<Vec<(String, String)>, String> {
            let mut stmt = conn.prepare(TURNS_SQL).map_err(sql)?;
            stmt.query_map([chunk], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)
        };
    // Each chunk on a read-only connection of its own, side by side. A ledger with no file
    // behind it (in memory) is read on this one connection.
    let file = conn.path().filter(|p| !p.is_empty()).map(Path::new);
    let packed: Vec<Vec<(String, String)>> = match file {
        Some(file) if chunks.len() > 1 => std::thread::scope(|scope| {
            let handles: Vec<_> = chunks
                .iter()
                .map(|chunk| {
                    scope.spawn(move || {
                        let c = crate::meter::open_read_only(file).map_err(sql)?;
                        c.busy_timeout(std::time::Duration::from_secs(5))
                            .map_err(sql)?;
                        read(&c, chunk)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| match h.join() {
                    Ok(r) => r,
                    Err(_) => Err("a thread reading turns panicked".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()
        })?,
        _ => chunks
            .iter()
            .map(|chunk| read(conn, chunk))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let (sessions_turns, classes) = parse_chunks(packed)?;

    let mut stmt = conn.prepare(BOUNDARIES_SQL).map_err(sql)?;
    let boundaries = stmt
        .query_map([&sessions], |r| {
            Ok(BoundaryRow {
                session_id: r.get(0)?,
                kind: r.get(1)?,
                ts_ms: r.get(2)?,
                source_file: r.get(3)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;

    let recent_turns: i64 = conn
        .query_row(RECENT_TURNS_SQL, [recent_since(now_ms)], |r| r.get(0))
        .map_err(sql)?;
    let (events_from, subagent_events_from, meter_agents_from): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = conn
        .query_row(COVERAGE_SQL, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(sql)?;
    let (valve_sessions, valve_paths) = valve_keys_json(&blocked_reads);
    let packed: Option<String> = conn
        .query_row(VALVE_READS_SQL, [valve_sessions, valve_paths], |r| r.get(0))
        .map_err(sql)?;
    let valve_reads = parse_valve_reads(packed.as_deref())?;
    let packed: Option<String> = conn
        .query_row(SCANNED_SQL, [&sessions], |r| r.get(0))
        .map_err(sql)?;
    let scanned = parse_scanned(packed.as_deref())?;

    Ok((
        Ledger {
            calls,
            sessions: sessions_turns,
            classes,
            turns: Vec::new(),
            boundaries,
            blocked_reads,
            recorded_blocks,
            scanned,
            recent_turns,
            events_from,
            subagent_events_from,
            valve_reads,
            meter_agents_from,
            gaps: spool_gap.into_iter().collect(),
        },
        Clock {
            now_ms,
            week_start_ms: week_start_s * 1000,
            week_start,
        },
    ))
}

/// The summary for the ledger `conn` holds, at `now_ms`.
pub fn summary(
    conn: &rusqlite::Connection,
    now_ms: i64,
    spool: Option<&Path>,
    facts: &super::Facts,
    cal: &dyn super::Calibrator,
) -> Result<super::SavingsSummary, String> {
    let (ledger, clock) = load(conn, now_ms, spool)?;
    Ok(super::summarize(&ledger, &clock, facts, cal))
}
