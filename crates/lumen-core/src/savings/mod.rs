//! What Lumen saved: one engine, one summary.
//!
//! Every figure the app, the CLI, the tray and the benchmarks show about savings comes from
//! [`summarize`], and nothing else computes one. Until 1.9.0 there were three: lumen-stats priced
//! a net value at Opus-class rates with a fixed number of rounds remaining and a fixed number of
//! rounds per call, and the benchmark harness priced a "$/call" with the outline sizing's own
//! fixed rounds. Each was one machine's averages multiplied out.
//!
//! The engine is pure. It takes typed rows (the Lumen calls in `read_events`, the turns of the
//! sessions they were made in, the compactions and rewinds in `session_events`, the Reads the
//! intercept blocked as the transcripts record them, the transcripts read for them, and the
//! escape valve's faults), a clock, two facts about the install and a
//! [`Calibrator`], and returns a [`SavingsSummary`]. The loaders that read those rows are thin and
//! share their SQL ([`load`]), so the app (sqlx, in lumen-stats) and the CLI (rusqlite, here)
//! read the same rows in the same order and get the same bytes.
//!
//! # Per call
//!
//! A Lumen call saves `S` tokens: what the conversation would otherwise have been given, less
//! what the call returned, signed (a reply larger than that is a loss). The counterfactual:
//! - the built-in Read, which returns at most [`READ_TOKEN_LIMIT`] tokens of a file in one call
//!   (Claude Code's documented default), so a larger file counts as that much, not the whole. A
//!   file over 256 KB is refused whole with an error, and the model then reads a page, so the cap
//!   holds there too; the refused Read, a round the counterfactual spends, is not credited. Until
//!   calibration lands the counts are cl100k_base's, so the cap is applied to those;
//! - for a repeat read's stub or delta, the answer Lumen would otherwise have sent again: its
//!   earlier answer to the same request in the same conversation, as the ledger recorded it.
//!   With no such answer in the ledger it counts as none, the whole file at most;
//! - for a `fresh=true` call after a stub, nothing: the stub already stood in for the answer it
//!   asks for again, so this call adds what it returns;
//! - for compress_logs on inline text, nothing: the text was in context already.
//!
//! Recorded in cl100k_base tokens, a proxy, and converted to Claude's tokens by the calibrator.
//!
//! **The consuming turn.** The call's session (and, inside a subagent, its agent: a subagent's
//! turns are those of its own transcript, `<session>/subagents/agent-<agent_id>.jsonl`, under
//! either path separator) is a chain of turns, `<synthetic>` messages left out. The first turn of
//! that chain at least one second after the call's recorded second is the round that read the
//! result: the ledger records a call to the second, and the turn that made the call can share it.
//! Its model prices the saving.
//!
//! **R**, rounds remaining: the turns of the same chain after the consuming turn, up to the first
//! of
//! - a compaction or rewind recorded in that transcript (a subagent's compaction in its own);
//! - a collapse: the first later turn whose prompt (input, cache writes and cache reads) is
//!   smaller than the turn's before it. A conversation's prompt only grows until context is taken
//!   out (a compaction, a rewind, tool results cleared), and whether the result went with it
//!   cannot be told, so R ends there. This catches every compaction the ledger never recorded,
//!   including one that leaves a summary larger than the prompt the result entered; inferred,
//!   and labelled so. A turn that switches to a model counting tokens differently can show a
//!   shrink that took nothing out: R is then short, never long;
//! - the end of the session. A session whose last turn is under an hour old may still be
//!   running: its R is a lower bound. A conversation that began before the daemon recorded events
//!   ([`Ledger::events_from`]) and shows no collapse after the result has no record to vouch for
//!   its end (a turn the ledger lacks could hide a compaction): its R may be too long, so it is
//!   labelled `sessionEndUnrecorded`, and only what the saving is worth at R = 0 is measured (see
//!   below).
//!
//! A resync (the daemon lost its place in a file) ends nothing: the lines after it are still read
//! for compaction records, which need no state, and a rewind it missed shows as a collapse.
//!
//! **Value** of saving `S` tokens: `S × (one-time price + Σ cache-read price over the R turns)`.
//! The one-time price is what the consuming turn paid to put new tokens in context: the cache
//! write at the duration it recorded (1-hour or 5-minute), the input price if it wrote no cache,
//! and the 5-minute price as a floor when it recorded no split. Each of the R turns is priced at
//! its own model, speed and region. Reading a token from cache on every later round is the least
//! a saved token would have cost: a round after the cache expired writes it again, at a higher
//! price, so this is a floor.
//!
//! **C**, the cost of one extra round: the consuming turn's own usage (input, cache writes, cache
//! reads, output) at its model, speed, region and cache duration.
//!
//! **Extra rounds**, against the counterfactual of the same session without Lumen, where the
//! model would have called the built-in Read once:
//! - a repeat read answered with a stub or a delta replaces a re-read one for one: none;
//! - a call made with `fresh=true` after a stub of the same request asked again because the stub
//!   was not enough: one, on evidence;
//! - a `recall_file`, or `smart_read(mode="full")`, after an outline of the same file in the same
//!   context: the counterfactual Read would have returned it at once, so one, on evidence;
//! - `compress_logs` on inline text: one, plus the output tokens of writing the text out, on
//!   evidence;
//! - a Read the intercept blocked: one, on evidence, priced at the turn of the blocked agent that
//!   read the block's message. The hook writes no row for a block (only a marker file of its
//!   own), but the transcript of the agent that tried the Read records it: the Read's call, then
//!   its result, an error holding the block's message. The daemon reads each one there
//!   ([`crate::intercept`], `intercept_blocks`), and records which transcripts it has read, from
//!   their first line through which turn of each session in them (`transcript_scans`). A
//!   conversation is read as far as its own transcript (`<session>.jsonl`, or the subagent's own
//!   under `subagents/`) was, and no further than any other file read with lines of it;
//! - any other call may have followed a blocked Read. Where a transcript records a block of the
//!   call's own file in the call's own conversation before it, no earlier call was answered by
//!   that block, and the transcript was read through the call, the block is the call's round, a
//!   line of its own, and the call carries none. Elsewhere whether one did cannot be told (a
//!   block of another file says nothing of this one, and a block the daemon did not record leaves
//!   no trace): one, counted (conservatively) and labelled modelled. The band's high end leaves
//!   these out;
//! - where no transcript records the block of a path (the transcript was deleted before the
//!   daemon read it, or has not been read yet), the escape valve's faults stand for it: a Read the
//!   intercept blocked and its escape valve then let through
//!   (`hook_fail_open/retry_escape_valve`): one, on evidence, priced at the turn that retried. The
//!   intercept blocks a path once per context (the session's, and each subagent's apart), and
//!   the valve logs a fault on every later Read of the same path, which costs nothing, so one is
//!   counted per session, agent and path, and a second only where the fault says the path was
//!   blocked again after smart_read or recall_file answered for it (`after_lumen_read`). A fault
//!   names its subagent from 1.9.0, so one written by 1.9.0 or later that names none is the main
//!   agent's; an older one is attributed by the meter's row for the same Read, which names the
//!   agent from 1.8.0: those blocks are on evidence. Faults nothing
//!   attributes stand for at most one block per agent that could have read the path then (a
//!   later fault is an allowed Read, and a new block can only be another agent's): the main
//!   agent, each subagent of the session with a turn within [`BLOCK_WINDOW_MS`] of the faults,
//!   and any agent the meter named on the path then, never more than there are faults. That many
//!   are counted, the first on evidence and the rest bounded and modelled, so the cost is never
//!   understated. Where a transcript does record the block of the path, the faults on it are its
//!   later Reads, counted beside it ([`Counts::valve_faults`]) and as nothing more: a fault whose
//!   agent is known is that agent's block's, and faults nothing attributes are bounded only by the
//!   agents whose transcripts were not read through their latest turn.
//!
//! **Net** = value − extra-round cost, signed. Never clamped.
//!
//! # Dollars: measured, modelled, tokens only
//!
//! A dollar is the price of the model that consumed the tokens, so a row with no consuming turn
//! of its own has no dollars at all. Each period's dollars come in three parts, which no surface
//! adds together:
//! - **measured** ([`Period::measured`]): every call priced at the turn that read it, less every
//!   extra round it cost, on evidence or modelled, and the blocked Reads, each priced at the turn
//!   that read the block (a transcript's) or that retried it (a fault's). This is the figure shown
//!   in bold, as "at least": every input is resolved the
//!   low way (a cache read is the least a kept token costs, unrecorded prices fall low on the
//!   saving and high on the round, modelled rounds are charged). Where R may be too long
//!   (`sessionEndUnrecorded`), the saving is taken at the least it can be worth: at R = 0 (the
//!   one-time price alone) for a saving, which only grows with R, and at R for a loss, which
//!   grows with R too. Its rounds are certain, and charged in full;
//! - **modelled** ([`Period::modelled`]): what those calls are worth beyond that, the cache reads
//!   over an R that may be too long. Shown beside the measured figure, labelled, never in it;
//! - **tokens only** ([`Period::unpriced`]): no consuming turn to price by (no session id, as on
//!   most rows before 1.1.5; no turn after the call; a subagent whose transcript is not in the
//!   ledger; a time that does not parse; a compaction before any turn read the result), or one
//!   whose model has no price (`rates::price_for`). Their tokens count; their dollars are not
//!   guessed.
//!
//! # Bands
//!
//! Every token and dollar figure is a band `{low, value, high}`. Tokens: the calibrator's band.
//! Dollars: `value` is the conservative central figure. Where a turn did not record its speed, its
//! inference region or its cache-write duration, the value side is priced at the lower rate
//! (standard speed, global routing, the 5-minute write) and the extra round at the higher one
//! (fast mode's prices, US-only inference's 1.1 times, the 1-hour write), and every modelled extra
//! round is counted. `low` takes the low end of the tokens as well; `high` prices both sides the
//! other way and leaves modelled extra rounds out, and is no upper bound where R is a lower bound
//! ([`Dollars::high_open`]).

mod chain;
pub mod load;
#[cfg(test)]
mod tests;
pub mod text;

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::intercept;
use crate::repeat::{
    ARM_TREATMENT, PLAN_FRESH, ROUTE_DELTA, ROUTE_RECALL_DELTA, ROUTE_RECALL_UNCHANGED,
    ROUTE_UNCHANGED,
};
use crate::settings::BillingMode;

pub use chain::{iso_ms, parse_ts_ms};

/// Bumped when a field of [`SavingsSummary`] changes meaning or name.
pub const SCHEMA: u32 = 1;

/// Every `routed_via` the MCP server writes. A row with one of these is a Lumen call; the hooks'
/// `builtin_read` and `bash_output` are not, nor is a route this build does not know (counted in
/// no figure rather than guessed at).
pub const CALL_ROUTES: &[&str] = &[
    "smart_read",
    "recall_file",
    "compress_logs",
    "smart_read_full",
    "would_inflate",
    "ranked_outline",
    "ranked_no_query",
    "ranked_no_defs",
    "ranked_not_worth_it",
    "ranked_too_slow",
    ROUTE_UNCHANGED,
    ROUTE_DELTA,
    ROUTE_RECALL_UNCHANGED,
    ROUTE_RECALL_DELTA,
];

/// The routes a repeat read is served on: an answer that replaces a re-read one for one.
pub const REPEAT_ROUTES: [&str; 4] = [
    ROUTE_UNCHANGED,
    ROUTE_DELTA,
    ROUTE_RECALL_UNCHANGED,
    ROUTE_RECALL_DELTA,
];

/// The path `compress_logs` records for inline text, which has no file behind it.
pub const INLINE_PATH: &str = "(inline)";

/// What one built-in Read returns at most, in Claude's tokens: Claude Code's documented default,
/// "By default one call returns at most 25,000 tokens, or the value you set in
/// `CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`, and refuses a whole file over 256 KB, so Claude
/// reads a larger file in pages with `offset` and `limit`"
/// (<https://code.claude.com/docs/en/tools-reference#large-files>, Claude Code 2.1.x). A file
/// larger than that never reached the context whole through a Read, so the counterfactual of a
/// call on it is this much. A file over 256 KB is refused with an error rather than given a first
/// page; the model then reads a page, so the cap holds, and the refused Read's round, which would
/// favour Lumen, is not credited. A user who raised the variable read more, and is credited less.
/// Until calibration lands the counts compared with it are cl100k_base's, a proxy for Claude's.
pub const READ_TOKEN_LIMIT: i64 = 25_000;

/// The window "recent" means for status and recommendation: no call in it is a finding.
pub const RECENT_DAYS: i64 = 7;

/// How far before a group of escape-valve faults no evidence attributes, and after it, a
/// subagent's turn makes it one of the agents that could have been blocked on the path.
pub const BLOCK_WINDOW_MS: i64 = 5 * 60 * 1000;

/// A session whose latest turn is younger than this may still be running.
pub const OPEN_SESSION_MS: i64 = 60 * 60 * 1000;

/// Per-call lines kept in [`CallLedger::recent`], newest first.
pub const LEDGER_LIMIT: usize = 200;

/// Calls in each of [`CallLedger::top_gains`] and [`CallLedger::top_losses`].
pub const TOP_N: usize = 5;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

// ── inputs ───────────────────────────────────────────────────────────────────

/// One Lumen call, as `read_events` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallRow {
    pub rowid: i64,
    pub ts: String,
    pub tool: String,
    pub path: String,
    pub routed_via: String,
    pub full_tokens: i64,
    pub returned_tokens: i64,
    pub saved_tokens: i64,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    /// The request's identity: the path for a smart_read, the path and selector for a
    /// recall_file. `None` on rows that predate it, which stand for their path.
    pub req_key: Option<String>,
    pub token_source: Option<String>,
    pub repeat_arm: Option<String>,
    pub repeat_plan: Option<String>,
}

/// A turn's model, speed and inference region: what prices it. The loaders name each one once
/// and the turns point at it, so a million turns are not a million copies of three strings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TurnClass {
    pub model: Option<String>,
    pub speed: Option<String>,
    /// `usage.inference_geo` as recorded: `us`, `global`, `not_available`, or `None`.
    pub inference_geo: Option<String>,
}

/// One billable request as the loaders read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedTurn {
    /// The row's rowid: the order of turns written in the same millisecond.
    pub id: i64,
    /// Unix milliseconds, as SQLite reads `turns.ts`; `None` when it cannot.
    pub ts_ms: Option<i64>,
    /// Index into [`Ledger::classes`].
    pub class: u32,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_5m: Option<i64>,
    pub cache_1h: Option<i64>,
    pub is_subagent: bool,
    /// The transcript, only where it may be a subagent's: the main agent's is not needed.
    pub source_file: Option<String>,
}

/// One session's turns, as the loaders read them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTurns {
    pub session_id: String,
    pub turns: Vec<LoadedTurn>,
}

/// One billable request, spelled out: the convenient form for rows made by hand. The engine
/// reads these beside the loaders' [`SessionTurns`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnRow {
    pub message_id: String,
    pub session_id: String,
    pub ts: String,
    pub model: Option<String>,
    pub speed: Option<String>,
    /// `usage.inference_geo` as recorded: `us`, `global`, `not_available`, or `None`.
    pub inference_geo: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_5m: Option<i64>,
    pub cache_1h: Option<i64>,
    pub is_subagent: bool,
    pub source_file: Option<String>,
}

/// A compaction or rewind, as `session_events` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundaryRow {
    pub session_id: String,
    pub kind: String,
    pub ts_ms: i64,
    pub source_file: Option<String>,
}

/// A Read the intercept blocked and its escape valve then let through: a round spent for nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedReadRow {
    pub ts: String,
    pub session_id: Option<String>,
    pub path: Option<String>,
    /// `after_lumen_read` when smart_read or recall_file had answered for the path first, so the
    /// Read it released had been blocked a second time, with a redirect to recall_file.
    pub detail: Option<String>,
    /// The subagent whose Read it was, recorded from 1.9.0. `None` is the main agent's, or a
    /// fault from before: the meter's row for the same Read says which ([`ValveReadRow`]).
    pub agent_id: Option<String>,
    /// The Lumen version whose hook wrote the fault: from 1.9.0 a fault that names no agent is
    /// the main agent's.
    pub version: Option<String>,
}

/// Whether a hook of `version` names the subagent on every fault it writes (1.9.0 and later), so
/// that a fault naming none is the main agent's.
fn names_its_agent(version: Option<&str>) -> bool {
    let Some(v) = version else {
        return false;
    };
    let mut parts = v.split(['.', '-', '+']).map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (1, 9),
        _ => false,
    }
}

/// A Read the intercept blocked, as the transcript of the agent that tried it records it
/// (`intercept_blocks`): the hook writes no row for a block, so the daemon reads them there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedBlockRow {
    pub session_id: String,
    /// The subagent whose transcript holds it; `None` for the main agent's.
    pub agent_id: Option<String>,
    /// As the hook named it, which is the path its escape-valve faults name.
    pub path: String,
    /// The block's `tool_result` record, unix ms.
    pub ts_ms: i64,
    /// `redirect`, `log`, or `served`: blocked again after smart_read or recall_file answered for
    /// the path, as the escape valve's fault for the Read it then let through says
    /// (`after_lumen_read`).
    pub kind: String,
}

/// A transcript the daemon read for blocks from its first line, through the latest turn of
/// `session_id` in it, at `through_ms` (`transcript_scans`): a block of that session it does not
/// record did not happen in it by then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScannedRow {
    pub source_file: String,
    pub session_id: String,
    /// The subagent whose transcript it is, by its path; `None` for the main agent's.
    pub agent_id: Option<String>,
    pub through_ms: i64,
}

/// A built-in Read the meter recorded (`builtin_read`) in a session with a blocked Read whose
/// fault names no agent: the Read the escape valve let through is metered too, with its agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValveReadRow {
    pub session_id: String,
    pub path: String,
    /// Unix milliseconds; `None` when the time cannot be read.
    pub ts_ms: Option<i64>,
    /// The subagent, recorded from 1.8.0; `None` is the main agent's from then on.
    pub agent_id: Option<String>,
}

/// Everything the engine reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    /// In the order they were written (rowid).
    pub calls: Vec<CallRow>,
    /// The turns of every session a call or a blocked Read names, as the loaders read them.
    pub sessions: Vec<SessionTurns>,
    /// What [`LoadedTurn::class`] points at.
    pub classes: Vec<TurnClass>,
    /// Turns spelled out, read beside `sessions`: what a test or a fixture builds.
    pub turns: Vec<TurnRow>,
    pub boundaries: Vec<BoundaryRow>,
    pub blocked_reads: Vec<BlockedReadRow>,
    /// The blocked Reads the transcripts record, in the order they were written.
    pub recorded_blocks: Vec<RecordedBlockRow>,
    /// The transcripts of the sessions the engine reads that the daemon has read for blocks.
    pub scanned: Vec<ScannedRow>,
    /// Main-agent turns of any session in the [`RECENT_DAYS`] before the clock's now.
    pub recent_turns: i64,
    /// The earliest compaction, rewind or resync the daemon recorded in a main transcript: the
    /// best stand-in the ledger holds for when it began recording them. A transcript gone by then
    /// left none; one still on disk was read from its first line. A conversation that began
    /// earlier and shows no event may have compacted unrecorded. `None`: nothing recorded.
    pub events_from: Option<i64>,
    /// The same for subagent transcripts, whose compactions are recorded from 1.9.0.
    pub subagent_events_from: Option<i64>,
    /// The meter's built-in Reads in the sessions of blocked Reads that name no agent.
    pub valve_reads: Vec<ValveReadRow>,
    /// The earliest row in `read_events` that names an agent: the meter recorded agents from then
    /// on, so a row after it that names none is the main agent's. Before it, none says whose.
    pub meter_agents_from: Option<i64>,
    /// What a loader could not read. Each makes every period partial.
    pub gaps: Vec<String>,
}

/// When the summary is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    /// Unix milliseconds.
    pub now_ms: i64,
    /// Monday 00:00 of the current ISO week in local time, as unix milliseconds.
    pub week_start_ms: i64,
    /// That Monday's local date, `YYYY-MM-DD`.
    pub week_start: String,
}

/// How the user pays, as the summary reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BillingView {
    Subscription,
    Api,
    /// Not asked yet.
    Unset,
    /// The settings file could not be read, or holds something else.
    Unreadable,
}

impl BillingView {
    pub fn of(setting: &Result<Option<BillingMode>, String>) -> Self {
        match setting {
            Ok(Some(BillingMode::Subscription)) => BillingView::Subscription,
            Ok(Some(BillingMode::Api)) => BillingView::Api,
            Ok(None) => BillingView::Unset,
            Err(_) => BillingView::Unreadable,
        }
    }
}

/// What the surface knows about the install, beyond the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub billing_mode: BillingView,
    /// Whether Claude Code's settings register Lumen's identity hook (the surface's existing
    /// predicate), `None` when they could not be read. The ledger's own evidence wins over it.
    pub identity_hook_registered: Option<bool>,
}

// ── calibration seam ─────────────────────────────────────────────────────────

/// A low, central and high estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Band<T> {
    pub low: T,
    pub value: T,
    pub high: T,
}

impl<T: Copy> Band<T> {
    pub fn exact(v: T) -> Self {
        Band {
            low: v,
            value: v,
            high: v,
        }
    }
}

impl Band<i64> {
    fn add(&mut self, o: Band<i64>) {
        self.low += o.low;
        self.value += o.value;
        self.high += o.high;
    }
}

impl Band<f64> {
    fn add(&mut self, o: Band<f64>) {
        self.low += o.low;
        self.value += o.value;
        self.high += o.high;
    }

    /// `self − cost`, the ends crossed: the low net is the low value less the high cost.
    fn less(self, cost: Band<f64>) -> Band<f64> {
        Band {
            low: self.low - cost.high,
            value: self.value - cost.value,
            high: self.high - cost.low,
        }
    }

    /// To the micro-dollar, so the wire carries no float noise and two loaders' sums agree.
    fn rounded(self) -> Band<f64> {
        let r = |x: f64| (x * 1e6).round() / 1e6;
        Band {
            low: r(self.low),
            value: r(self.value),
            high: r(self.high),
        }
    }
}

/// What a calibrator says about itself, for the method section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationInfo {
    /// False while counts are cl100k_base's with no conversion: every token figure is then the
    /// proxy's, and its band does not include the proxy's error.
    pub calibrated: bool,
    pub as_of: Option<String>,
    pub note: String,
}

/// Converts Lumen's cl100k_base counts into Claude's, by content type (from the path) and model.
///
/// The seam the calibration table plugs into. Until it does, [`Uncalibrated`] passes counts
/// through and says so. `Sync`, so an async loader can hold one across an await on any thread.
pub trait Calibrator: Sync {
    /// Claude's count of what Lumen counted as `cl100k` tokens of the file at `path`, read by
    /// `model` (`None` when no turn names one).
    fn claude_tokens(&self, cl100k: u64, path: &str, model: Option<&str>) -> Band<u64>;
    fn info(&self) -> CalibrationInfo;
}

/// The identity: a cl100k_base count is reported as itself, with a zero-width band, and the
/// summary says the counts are uncalibrated.
pub struct Uncalibrated;

impl Calibrator for Uncalibrated {
    fn claude_tokens(&self, cl100k: u64, _path: &str, _model: Option<&str>) -> Band<u64> {
        Band::exact(cl100k)
    }

    fn info(&self) -> CalibrationInfo {
        CalibrationInfo {
            calibrated: false,
            as_of: None,
            note: "Uncalibrated: token figures are cl100k_base counts, OpenAI's tokenizer, a \
                   proxy for Claude's. Claude's count of the same text differs, and the bands \
                   do not yet include that difference."
                .to_string(),
        }
    }
}

/// A signed count through the calibrator: a loss is calibrated as its size and keeps its sign,
/// with the ends swapped so `low` stays the lower.
fn calibrated(cal: &dyn Calibrator, n: i64, path: &str, model: Option<&str>) -> Band<i64> {
    let b = cal.claude_tokens(n.unsigned_abs(), path, model);
    let s = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    if n >= 0 {
        Band {
            low: s(b.low),
            value: s(b.value),
            high: s(b.high),
        }
    } else {
        Band {
            low: -s(b.high),
            value: -s(b.value),
            high: -s(b.low),
        }
    }
}

// ── output ───────────────────────────────────────────────────────────────────

/// What Lumen saved, as every surface shows it. The frontend renders this and computes nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavingsSummary {
    pub schema: u32,
    /// When it was computed, ISO-8601 UTC.
    pub as_of: String,
    pub billing_mode: BillingView,
    /// The current ISO week: Monday 00:00 local time to now.
    pub week: Period,
    pub all_time: Period,
    pub status: Status,
    /// At most one thing to do, and only where the evidence names one.
    pub recommendation: Option<Recommendation>,
    pub method: Method,
    pub ledger: CallLedger,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    /// The period's start, ISO-8601 UTC: the week's local Monday 00:00, or the first entry of all
    /// time (`None` when there is none).
    pub from: Option<String>,
    pub to: String,
    /// Claude tokens kept out of context, net of any added. Signed.
    pub tokens_saved: Band<i64>,
    /// Claude tokens the calls' files held: what reading them whole would have put in context.
    pub tokens_full: Band<i64>,
    /// `tokens_saved / tokens_full` at the central figures, in percent; `None` with nothing read.
    pub share_pct: Option<f64>,
    /// The figure shown in bold, as "at least": the dollars measured from the calls' own turns.
    /// `None` when no row of the period could be measured. See the module docs.
    pub measured: Option<Dollars>,
    /// What calls whose R may be too long are worth beyond their worth at R = 0, which is in
    /// `measured`. Shown beside it, labelled modelled, never added to it. `None` when nothing is.
    pub modelled: Option<Modelled>,
    /// Rows with tokens and no dollars.
    pub unpriced: Unpriced,
    /// What the turns behind the dollars did not record, in the app's own words
    /// (`rates::Shortfall::describe`): each resolved the low way.
    pub usd_shortfalls: Vec<String>,
    pub counts: Counts,
    pub data_quality: DataQuality,
}

/// One part of a period's dollars.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dollars {
    /// Calls and blocked Reads in it.
    pub rows: i64,
    /// Value of the tokens saved.
    pub gross: Band<f64>,
    /// Cost of the extra rounds.
    pub extra_rounds: Band<f64>,
    /// `gross` less `extra_rounds`, signed.
    pub net: Band<f64>,
    /// `net.high` is no upper bound: an R in it is a lower bound (a session that may still be
    /// running), or may be too long and was taken at 0 (the rest is in the modelled figure).
    pub high_open: bool,
}

/// What a period's calls may be worth beyond the measured figure: the cache reads over an R
/// that may be too long.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Modelled {
    /// Calls it comes from.
    pub rows: i64,
    pub value: Band<f64>,
}

/// Rows with tokens and no dollars: no consuming turn to price them by, or one whose model has
/// no price.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unpriced {
    pub calls: i64,
    pub blocked_reads: i64,
    /// The calls' tokens saved, counted in `tokens_saved` with every other call's.
    pub tokens: Band<i64>,
    /// Rows with no session recorded (most before 1.1.5).
    pub no_session: i64,
    /// Rows with a session and no turn in the ledger that read them.
    pub no_turn: i64,
    /// Rows whose consuming model has no price.
    pub no_price: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    /// Lumen calls.
    pub calls: i64,
    /// Reads the intercept blocked, each an extra round: those a transcript records, and those
    /// inferred from escape-valve faults where none does. The next two and
    /// [`Counts::blocked_reads_bounded`] split them.
    pub blocked_reads: i64,
    /// Recorded in the transcript of the agent that tried the Read: on evidence, exactly.
    pub blocked_reads_from_transcripts: i64,
    /// Inferred from escape-valve faults no transcript record accounts for, on evidence: the fault
    /// or the meter's row for its Read names the agent, or it is the first of its path.
    pub blocked_reads_from_faults: i64,
    /// Calls whose R and C came from their own turn chain.
    pub derived_pairs: i64,
    /// Calls whose token count is not known to be measured.
    pub unverified_provenance_rows: i64,
    pub extra_rounds_evidenced: i64,
    pub extra_rounds_modelled: i64,
    /// Derived pairs whose R is a lower bound: the session may still be running.
    pub rounds_lower_bound_rows: i64,
    /// Derived pairs whose R may be too long: a conversation from before the daemon recorded
    /// events, with no event and no collapse to end it.
    pub rounds_unrecorded_rows: i64,
    /// Derived pairs whose R a collapse ended (inferred, not recorded).
    pub rounds_collapse_rows: i64,
    /// The escape-valve faults behind the blocked Reads: one is logged on every later Read of a
    /// path already blocked, which costs nothing. Where a transcript records the block, they
    /// corroborate it.
    pub valve_faults: i64,
    /// Blocked Reads counted because faults no evidence attributes to an agent could stand for
    /// them: one per agent that could have read the path then, beyond the first; modelled, at the
    /// most there could have been.
    pub blocked_reads_bounded: i64,
    /// Calls whose time could not be read: in all time, in no week.
    pub unreadable_time_rows: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DataQuality {
    /// Every row has a derived, priced pair and a measured count.
    Complete,
    Partial {
        reason: String,
    },
    /// Nothing recorded in the period.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    Working,
    NotWorking { reason: String },
    NoData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    /// Stable, for the frontend to key on: `blocksWithoutSavings`, `noCalls`, `identityHook`,
    /// `unplacedCalls`, `compactsEarly`, `smallSavings`.
    pub code: &'static str,
    pub text: String,
}

/// How every figure was made, for "How it's measured" and `lumen report`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
    pub prices_as_of: &'static str,
    pub calibration: CalibrationInfo,
    pub week: String,
    pub counterfactual: &'static str,
    pub rounds: &'static str,
    pub round_cost: &'static str,
    pub extra_rounds: &'static str,
    /// How a turn that did not record its speed, region or cache duration is priced.
    pub floors: &'static str,
    /// What goes in the measured, the modelled and the tokens-only parts.
    pub parts: &'static str,
}

/// The per-call ledger behind the figures.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLedger {
    /// Newest first, at most [`LEDGER_LIMIT`].
    pub recent: Vec<CallLine>,
    /// Lines older than those, left out of `recent`.
    pub omitted: i64,
    /// All time, by net dollars (by tokens when nothing is priced), at most [`TOP_N`].
    pub top_gains: Vec<CallLine>,
    pub top_losses: Vec<CallLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LineKind {
    Call,
    BlockedRead,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLine {
    pub ts: String,
    pub kind: LineKind,
    /// `smart_read`, `recall_file` or `compress_logs`; `None` for a blocked Read.
    pub tool: Option<&'static str>,
    pub route: Option<String>,
    pub path: Option<String>,
    pub in_week: bool,
    /// What is counted as saved, in cl100k_base tokens, against `saved_against`.
    pub saved_cl100k: i64,
    /// What the ledger recorded, against the whole file.
    pub recorded_saved_cl100k: i64,
    /// `wholeFile` (a Read, at most [`READ_TOKEN_LIMIT`]); `previousAnswer` for a repeat read,
    /// against the answer Lumen would have re-sent; `answerNotInLedger` for one whose earlier
    /// answer is not in the ledger (counted as none); `alreadyInContext` for inline text;
    /// `askedAgain` for a `fresh=true` call after a stub.
    pub saved_against: &'static str,
    pub full_cl100k: i64,
    /// The file is larger than a Read returns, so it counts as [`READ_TOKEN_LIMIT`].
    pub read_capped: bool,
    pub tokens_saved: Band<i64>,
    /// `measured`, or `unverified` for a count of unknown provenance.
    pub provenance: &'static str,
    pub pair: PairView,
    pub extra_rounds: i64,
    /// `repeatAnswer`, `followUp`, `freshAfterStub`, `inlineText`, `blockNotRecorded` (a call
    /// that may have followed a block no transcript records: modelled) or `blockRecorded` (one that
    /// followed a recorded block of its file, which is its round: none); for a blocked Read,
    /// `blockedReadRecorded` (a transcript records it), `blockedRead` (a fault stands for it) or
    /// `blockedReadBounded` (one of the most that faults no evidence attributes could).
    pub extra_round_basis: &'static str,
    pub extra_round_evidenced: bool,
    /// `measured` when the line has dollars, `none` when it has tokens only. Every priced line's
    /// `net_usd` is in the measured figure.
    pub dollars: &'static str,
    /// What the saving is worth at the line's R that it is not at R = 0, where R may be too long
    /// (`sessionEndUnrecorded`): in the period's modelled figure, not in `value_usd` or `net_usd`.
    pub modelled_value_usd: Option<Band<f64>>,
    pub value_usd: Option<Band<f64>>,
    pub extra_round_usd: Option<Band<f64>>,
    pub net_usd: Option<Band<f64>>,
    /// What the turns pricing this line did not record, where it changes the price: `speed`,
    /// `region`, `cacheDuration`. Any makes `net_usd.value` a floor.
    pub pricing_unknown: Vec<&'static str>,
}

/// The `detail` the intercept writes on an escape-valve fault for a path smart_read or
/// recall_file had answered for (`crates/lumen-mcp/src/hook.rs`).
const AFTER_LUMEN_READ: &str = "after_lumen_read";

/// Where a line's R and C came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairView {
    /// `derived`, or why not (the line then has tokens and no dollars): `noSession`,
    /// `unreadableTime`, `noFollowingTurn`, `clearedBeforeRead`, `subagentNotInLedger`.
    pub basis: &'static str,
    /// The consuming turn's model.
    pub model: Option<String>,
    pub speed: Option<String>,
    /// The consuming turn's `inference_geo`.
    pub region: Option<String>,
    /// R.
    pub rounds: Option<i64>,
    /// What ended R: `compaction`, `rewind`, `collapse` (inferred), `sessionEnd`,
    /// `sessionOpen` (a lower bound) or `sessionEndUnrecorded` (may be too long).
    pub rounds_bound: Option<&'static str>,
    /// R cannot be too long.
    pub rounds_trusted: bool,
    /// The cache duration the consuming turn wrote at: `1h`, `5m`, `mixed`, `unrecorded`,
    /// `uncached`.
    pub ttl: Option<&'static str>,
    /// Some of the R turns had no price and added nothing to the value.
    pub some_rounds_unpriced: bool,
}

// ── the engine ───────────────────────────────────────────────────────────────

/// Which tool a call was, from the tool name or, on rows that predate it, the route.
fn tool_of(call: &CallRow) -> Option<&'static str> {
    if let Some(t) = crate::identity::short_name(&call.tool) {
        return Some(t);
    }
    let r = call.routed_via.as_str();
    if r.starts_with("smart_read") || r.starts_with("ranked_") {
        Some("smart_read")
    } else if r.starts_with("recall_file") {
        Some("recall_file")
    } else if r == "compress_logs" {
        Some("compress_logs")
    } else {
        None
    }
}

/// An answer that puts a file's outline in context, which a later `recall_file` follows up.
fn is_outline(tool: Option<&str>, route: &str) -> bool {
    tool == Some("smart_read") && route != "smart_read_full"
}

/// Why a line costs the rounds it does. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rounds {
    RepeatAnswer,
    FreshAfterStub,
    FollowUp,
    InlineText,
    BlockNotRecorded,
    /// A call that followed a block of the same file in the same conversation, which a transcript
    /// records: the block is a line of its own ([`Rounds::BlockedReadRecorded`]), so the call
    /// carries none.
    BlockRecorded,
    BlockedRead,
    /// A blocked Read that may have happened: one of the most the faults no evidence attributes
    /// could stand for. Counted, so the cost is not understated, and modelled.
    BlockedReadBounded,
    /// A blocked Read the transcript of the agent that tried it records.
    BlockedReadRecorded,
}

impl Rounds {
    fn basis(self) -> &'static str {
        match self {
            Rounds::RepeatAnswer => "repeatAnswer",
            Rounds::FreshAfterStub => "freshAfterStub",
            Rounds::FollowUp => "followUp",
            Rounds::InlineText => "inlineText",
            Rounds::BlockNotRecorded => "blockNotRecorded",
            Rounds::BlockRecorded => "blockRecorded",
            Rounds::BlockedRead => "blockedRead",
            Rounds::BlockedReadBounded => "blockedReadBounded",
            Rounds::BlockedReadRecorded => "blockedReadRecorded",
        }
    }

    /// (evidenced, modelled) extra rounds.
    fn count(self) -> (i64, i64) {
        match self {
            Rounds::RepeatAnswer | Rounds::BlockRecorded => (0, 0),
            Rounds::BlockNotRecorded | Rounds::BlockedReadBounded => (0, 1),
            _ => (1, 0),
        }
    }
}

/// A request or a file, in its conversation: session, subagent, and path or request key.
type Context<'a> = (&'a str, Option<&'a str>, &'a str);

fn rounds_for(
    call: &CallRow,
    tool: Option<&str>,
    outlined: &HashSet<Context<'_>>,
    key: Option<&Context<'_>>,
    stubbed: bool,
) -> Rounds {
    if REPEAT_ROUTES.contains(&call.routed_via.as_str()) {
        return Rounds::RepeatAnswer;
    }
    if tool == Some("compress_logs") && call.path == INLINE_PATH {
        return Rounds::InlineText;
    }
    // Asked again with `fresh=true` after a stub of the same request: the stub was not enough.
    if stubbed
        && call.repeat_plan.as_deref() == Some(PLAN_FRESH)
        && call.repeat_arm.as_deref() == Some(ARM_TREATMENT)
    {
        return Rounds::FreshAfterStub;
    }
    let follows = tool == Some("recall_file") || call.routed_via == "smart_read_full";
    if follows && key.is_some_and(|k| outlined.contains(k)) {
        return Rounds::FollowUp;
    }
    Rounds::BlockNotRecorded
}

/// One line before it is priced.
/// What a call's saving is counted against: what the conversation would have been given without
/// the answer Lumen gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Against {
    /// The whole file, which the built-in Read would have put in context.
    WholeFile,
    /// A repeat read's stub or delta: Lumen would otherwise have sent its earlier answer to the
    /// same request again (as 1.7.0 did by default), not the whole file. That answer's size is
    /// the one the ledger recorded for it, earlier in the same conversation.
    PreviousAnswer,
    /// A repeat read whose earlier answer is not in the ledger: no saving is counted, and the
    /// whole file is the band's high end.
    AnswerNotInLedger,
    /// Inline text for compress_logs: it was in context already, so nothing.
    AlreadyInContext,
    /// A `fresh=true` call after a stub: the stub already stood in for the answer it asks for
    /// again, so against nothing.
    AskedAgain,
}

impl Against {
    fn as_str(self) -> &'static str {
        match self {
            Against::WholeFile => "wholeFile",
            Against::PreviousAnswer => "previousAnswer",
            Against::AnswerNotInLedger => "answerNotInLedger",
            Against::AlreadyInContext => "alreadyInContext",
            Against::AskedAgain => "askedAgain",
        }
    }
}

struct Draft {
    ts: String,
    ts_ms: Option<i64>,
    kind: LineKind,
    tool: Option<&'static str>,
    route: Option<String>,
    path: Option<String>,
    /// As the ledger recorded it, against the whole file.
    recorded_cl100k: i64,
    against: Against,
    full_cl100k: i64,
    returned_cl100k: i64,
    /// The earlier answer a repeat read stands in for, when the ledger has it.
    previous_cl100k: Option<i64>,
    unverified: bool,
    rounds: Rounds,
    pair: chain::Outcome,
    /// For a blocked Read, the escape-valve faults it stands for.
    valve_faults: i64,
}

/// Value of a signed token band at a value-per-token band, conservatively: a gain at the floor
/// price and a loss at the ceiling in `low` and `value`, the reverse in `high`.
fn value_of(s: Band<i64>, p: &chain::Price) -> Band<f64> {
    let pessimistic = |n: i64| n as f64 * if n >= 0 { p.v_lo } else { p.v_hi };
    Band {
        low: pessimistic(s.low),
        value: pessimistic(s.value),
        high: s.high as f64 * if s.high >= 0 { p.v_hi } else { p.v_lo },
    }
}

/// Cost of a line's extra rounds, and for inline text the output tokens of writing it out.
fn cost_of(rounds: Rounds, p: &chain::Price, written: Band<i64>) -> Band<f64> {
    let (evidenced, modelled) = rounds.count();
    let mut cost = Band {
        low: evidenced as f64 * p.c_lo,
        value: (evidenced + modelled) as f64 * p.c_hi,
        high: (evidenced + modelled) as f64 * p.c_hi,
    };
    if rounds == Rounds::InlineText {
        cost.add(Band {
            low: written.low as f64 * p.out_lo,
            value: written.value as f64 * p.out_hi,
            high: written.high as f64 * p.out_hi,
        });
    }
    cost
}

/// Why a line has no dollars: no session, no turn that read it, or no price for its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoPrice {
    Session,
    /// A session, and no turn in the ledger that read the line.
    Turn,
    /// The consuming model has no price.
    ModelPrice,
}

/// Whether a line's dollars are in the measured figure, or it has tokens only. See the module
/// docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Measured,
    Unpriced(NoPrice),
}

impl Part {
    fn as_str(self) -> &'static str {
        match self {
            Part::Measured => "measured",
            Part::Unpriced(_) => "none",
        }
    }
}

/// A priced line, and what it adds to its periods.
struct Priced {
    line: CallLine,
    ts_ms: Option<i64>,
    full: Band<i64>,
    derived: bool,
    part: Part,
    lower_bound: bool,
    /// R may be too long (`sessionEndUnrecorded`).
    rounds_untrusted: bool,
    unverified: bool,
    /// For a blocked Read, the escape-valve faults it stands for.
    valve_faults: i64,
    rounds: Rounds,
    /// What the turns pricing it did not record.
    unknowns: chain::Unknowns,
    /// The consuming model, when it has no price.
    unpriced_model: Option<String>,
}

fn price(draft: Draft, cal: &dyn Calibrator, week_start: i64) -> Priced {
    // A blocked Read saved nothing, so it has no R and no cache duration to show: only the
    // round it cost, priced at the turn that retried.
    let blocked = draft.kind == LineKind::BlockedRead;
    let (model, derived_price, view) = match &draft.pair {
        chain::Outcome::Derived(p) => (
            p.model.clone(),
            p.price.clone(),
            PairView {
                basis: "derived",
                model: p.model.clone(),
                speed: p.speed.clone(),
                region: p.geo.clone(),
                rounds: (!blocked).then_some(p.rounds),
                rounds_bound: (!blocked).then(|| p.bound.as_str()),
                rounds_trusted: blocked || p.rounds_trusted,
                ttl: (!blocked).then(|| p.ttl.as_str()),
                some_rounds_unpriced: p.price.as_ref().is_some_and(|x| x.some_rounds_unpriced),
            },
        ),
        chain::Outcome::NoPair(why) => (
            None,
            None,
            PairView {
                basis: why.as_str(),
                model: None,
                speed: None,
                region: None,
                rounds: None,
                rounds_bound: None,
                rounds_trusted: false,
                ttl: None,
                some_rounds_unpriced: false,
            },
        ),
    };
    let path = draft.path.as_deref().unwrap_or("").to_string();
    let cal_n = |n: i64| calibrated(cal, n, &path, model.as_deref());
    let none = Band::exact(0);
    // A saving is the counterfactual less the reply, end with end: one file's tokens and its
    // reply's are the same kind of text, so a calibrator's ratio is the same for both.
    let less = |f: Band<i64>, r: Band<i64>| {
        let (a, b) = (f.low - r.low, f.high - r.high);
        Band {
            low: a.min(b),
            value: f.value - r.value,
            high: a.max(b),
        }
    };
    let returned = cal_n(draft.returned_cl100k);
    let negative = |b: Band<i64>| Band {
        low: -b.high,
        value: -b.value,
        high: -b.low,
    };
    // `would`: what the conversation would have been given, in Claude's tokens.
    let (tokens, would, saved_cl100k, read_capped) = match draft.against {
        Against::WholeFile => {
            let f = cal_n(draft.full_cl100k);
            let cap = |n: i64| n.min(READ_TOKEN_LIMIT);
            let would = Band {
                low: cap(f.low),
                value: cap(f.value),
                high: cap(f.high),
            };
            (
                less(would, returned),
                would,
                cap(draft.full_cl100k) - draft.returned_cl100k,
                f.value > READ_TOKEN_LIMIT,
            )
        }
        Against::PreviousAnswer => {
            let previous = draft
                .previous_cl100k
                .map_or(draft.full_cl100k, |p| p.min(draft.full_cl100k));
            let would = cal_n(previous);
            (
                less(would, returned),
                would,
                previous - draft.returned_cl100k,
                false,
            )
        }
        // Unknown, and positive (the server answers with a stub only when it is the smaller):
        // none counted, the whole file at most.
        Against::AnswerNotInLedger => (
            Band {
                low: 0,
                value: 0,
                high: cal_n(draft.recorded_cl100k).high.max(0),
            },
            none,
            0,
            false,
        ),
        Against::AlreadyInContext | Against::AskedAgain => {
            (negative(returned), none, -draft.returned_cl100k, false)
        }
    };
    // What inline text cost to write out as the call's argument: the whole text, as output.
    let written = cal_n(draft.full_cl100k);
    // The consuming turn's own price, or none: a row with no consuming turn is not priced from
    // anyone else's.
    let price = derived_price;
    let derived = matches!(draft.pair, chain::Outcome::Derived(_));
    let lower_bound = !blocked
        && matches!(&draft.pair, chain::Outcome::Derived(p) if p.bound == chain::Bound::SessionOpen);
    // R may be too long: the line's dollars are then modelled, never measured.
    let rounds_untrusted =
        !blocked && matches!(&draft.pair, chain::Outcome::Derived(p) if !p.rounds_trusted);
    let part = match (&draft.pair, &price) {
        (chain::Outcome::NoPair(chain::Why::NoSession), _) => Part::Unpriced(NoPrice::Session),
        (chain::Outcome::NoPair(_), _) => Part::Unpriced(NoPrice::Turn),
        (chain::Outcome::Derived(_), None) => Part::Unpriced(NoPrice::ModelPrice),
        (chain::Outcome::Derived(_), Some(_)) => Part::Measured,
    };
    let (value_usd, modelled_value_usd, extra_round_usd, net_usd) = match &price {
        Some(p) => {
            let full = value_of(tokens, p);
            // Where R may be too long, the floor of the value is the least of its worth at R = 0
            // and at R: a saving grows with R, so R = 0; a loss grows with R too, so R. What the
            // saving is worth beyond that is modelled. The cost is certain either way.
            let (value, modelled) = if rounds_untrusted {
                let at_zero = value_of(
                    tokens,
                    &chain::Price {
                        v_lo: p.once_lo,
                        v_hi: p.once_hi,
                        ..p.clone()
                    },
                );
                let floor = Band {
                    low: full.low.min(at_zero.low),
                    value: full.value.min(at_zero.value),
                    high: full.high.min(at_zero.high),
                };
                let rest = Band {
                    low: full.low - floor.low,
                    value: full.value - floor.value,
                    high: full.high - floor.high,
                };
                (floor, Some(rest.rounded()))
            } else {
                (full, None)
            };
            let cost = cost_of(draft.rounds, p, written);
            (
                Some(value.rounded()),
                modelled,
                Some(cost.rounded()),
                Some(value.less(cost).rounded()),
            )
        }
        None => (None, None, None, None),
    };
    let (evidenced, modelled_rounds) = draft.rounds.count();
    let line = CallLine {
        ts: draft.ts,
        kind: draft.kind,
        tool: draft.tool,
        route: draft.route,
        path: draft.path,
        in_week: draft.ts_ms.is_some_and(|t| t >= week_start),
        saved_cl100k,
        recorded_saved_cl100k: draft.recorded_cl100k,
        saved_against: draft.against.as_str(),
        full_cl100k: draft.full_cl100k,
        read_capped,
        tokens_saved: tokens,
        provenance: if draft.unverified {
            "unverified"
        } else {
            "measured"
        },
        pair: view,
        extra_rounds: evidenced + modelled_rounds,
        extra_round_basis: draft.rounds.basis(),
        extra_round_evidenced: modelled_rounds == 0,
        dollars: part.as_str(),
        modelled_value_usd,
        value_usd,
        extra_round_usd,
        net_usd,
        pricing_unknown: price.as_ref().map_or_else(Vec::new, |p| p.unknowns.names()),
    };
    Priced {
        unknowns: price
            .as_ref()
            .map_or_else(chain::Unknowns::default, |p| p.unknowns),
        unpriced_model: match (&price, &line.pair.model) {
            (None, Some(m)) => Some(m.clone()),
            _ => None,
        },
        part,
        line,
        ts_ms: draft.ts_ms,
        full: would,
        derived,
        lower_bound,
        rounds_untrusted,
        unverified: draft.unverified,
        valve_faults: draft.valve_faults,
        rounds: draft.rounds,
    }
}

/// One part of a period's dollars, summed.
#[derive(Default)]
struct DollarsAcc {
    rows: i64,
    gross: Band<f64>,
    extra: Band<f64>,
    net: Band<f64>,
    high_open: bool,
}

impl DollarsAcc {
    fn add(&mut self, v: Band<f64>, x: Band<f64>, n: Band<f64>, high_open: bool) {
        self.rows += 1;
        self.gross.add(v);
        self.extra.add(x);
        self.net.add(n);
        self.high_open |= high_open;
    }

    fn done(self) -> Option<Dollars> {
        (self.rows > 0).then(|| Dollars {
            rows: self.rows,
            gross: self.gross.rounded(),
            extra_rounds: self.extra.rounded(),
            net: self.net.rounded(),
            high_open: self.high_open,
        })
    }
}

/// One period's sums.
#[derive(Default)]
struct Acc {
    tokens: Band<i64>,
    full: Band<i64>,
    measured: DollarsAcc,
    modelled: Option<Modelled>,
    unpriced: Unpriced,
    unknowns: chain::Unknowns,
    unpriced_models: Vec<String>,
    counts: Counts,
    first: Option<i64>,
}

impl Acc {
    fn add(&mut self, p: &Priced) {
        let c = &mut self.counts;
        match p.line.kind {
            LineKind::Call => {
                c.calls += 1;
                self.tokens.add(p.line.tokens_saved);
                self.full.add(p.full);
            }
            LineKind::BlockedRead => {
                c.blocked_reads += 1;
                c.valve_faults += p.valve_faults;
                match p.rounds {
                    Rounds::BlockedReadRecorded => c.blocked_reads_from_transcripts += 1,
                    Rounds::BlockedReadBounded => c.blocked_reads_bounded += 1,
                    _ => c.blocked_reads_from_faults += 1,
                }
            }
        }
        if p.ts_ms.is_none() {
            c.unreadable_time_rows += 1;
        }
        if p.derived && p.line.kind == LineKind::Call {
            c.derived_pairs += 1;
        }
        if let Some(m) = &p.unpriced_model
            && !self.unpriced_models.contains(m)
        {
            self.unpriced_models.push(m.clone());
        }
        if p.unverified {
            c.unverified_provenance_rows += 1;
        }
        if p.lower_bound {
            c.rounds_lower_bound_rows += 1;
        }
        if p.rounds_untrusted {
            c.rounds_unrecorded_rows += 1;
        }
        if matches!(p.line.pair.rounds_bound, Some("collapse")) {
            c.rounds_collapse_rows += 1;
        }
        if p.line.extra_round_evidenced {
            c.extra_rounds_evidenced += p.line.extra_rounds;
        } else {
            c.extra_rounds_modelled += p.line.extra_rounds;
        }
        let priced = match (p.line.value_usd, p.line.extra_round_usd, p.line.net_usd) {
            (Some(v), Some(x), Some(n)) => Some((v, x, n)),
            _ => None,
        };
        match (p.part, priced) {
            (Part::Measured, Some((v, x, n))) => {
                // An R that is a lower bound, or one whose worth beyond R = 0 is left to the
                // modelled figure: either way the high end is no bound.
                self.measured
                    .add(v, x, n, p.lower_bound || p.rounds_untrusted);
                self.unknowns = self.unknowns.or(p.unknowns);
                if let Some(rest) = p.line.modelled_value_usd
                    && (rest.low != 0.0 || rest.value != 0.0 || rest.high != 0.0)
                {
                    let m = self.modelled.get_or_insert(Modelled {
                        rows: 0,
                        value: Band::default(),
                    });
                    m.rows += 1;
                    m.value.add(rest);
                }
            }
            (part, _) => {
                let u = &mut self.unpriced;
                match p.line.kind {
                    LineKind::Call => {
                        u.calls += 1;
                        u.tokens.add(p.line.tokens_saved);
                    }
                    LineKind::BlockedRead => u.blocked_reads += 1,
                }
                match part {
                    Part::Unpriced(NoPrice::Session) => u.no_session += 1,
                    Part::Unpriced(NoPrice::Turn) => u.no_turn += 1,
                    _ => u.no_price += 1,
                }
            }
        }
        if let Some(t) = p.ts_ms {
            self.first = Some(self.first.map_or(t, |f| f.min(t)));
        }
    }

    fn period(self, from: Option<String>, to: &str, gaps: &[String]) -> Period {
        let c = &self.counts;
        let u = &self.unpriced;
        let quality = if c.calls == 0 && c.blocked_reads == 0 {
            DataQuality::Empty
        } else {
            let rows = |n: i64| {
                if n == 1 {
                    "1 row".to_string()
                } else {
                    format!("{n} rows")
                }
            };
            let mut why: Vec<String> = Vec::new();
            if u.no_session > 0 {
                why.push(format!(
                    "{} with no session recorded, so tokens and no dollars",
                    rows(u.no_session)
                ));
            }
            if u.no_turn > 0 {
                why.push(format!(
                    "{} with no turn in the ledger that read it, so tokens and no dollars",
                    rows(u.no_turn)
                ));
            }
            if u.no_price > 0 {
                why.push(format!(
                    "{} read by a model with no price, so tokens and no dollars",
                    rows(u.no_price)
                ));
            }
            if c.unverified_provenance_rows > 0 {
                why.push(format!(
                    "{} with a token count not known to be measured",
                    rows(c.unverified_provenance_rows)
                ));
            }
            if c.unreadable_time_rows > 0 {
                why.push(format!(
                    "{} with a time that could not be read",
                    rows(c.unreadable_time_rows)
                ));
            }
            why.extend(gaps.iter().cloned());
            if why.is_empty() {
                DataQuality::Complete
            } else {
                DataQuality::Partial {
                    reason: why.join("; "),
                }
            }
        };
        let mut usd_shortfalls: Vec<String> = self
            .unknowns
            .shortfalls()
            .iter()
            .map(crate::rates::Shortfall::describe)
            .collect();
        if !self.unpriced_models.is_empty() {
            let mut models = self.unpriced_models.clone();
            models.sort();
            usd_shortfalls.push(crate::rates::Shortfall::NoPrice(models).describe());
        }
        let share_pct = (self.full.value != 0).then(|| {
            let pct = 100.0 * self.tokens.value as f64 / self.full.value as f64;
            (pct * 10.0).round() / 10.0
        });
        Period {
            from,
            to: to.to_string(),
            tokens_saved: self.tokens,
            tokens_full: self.full,
            share_pct,
            measured: self.measured.done(),
            modelled: self.modelled.map(|m| Modelled {
                rows: m.rows,
                value: m.value.rounded(),
            }),
            unpriced: self.unpriced,
            usd_shortfalls,
            counts: self.counts,
            data_quality: quality,
        }
    }
}

const COUNTERFACTUAL_RULE: &str = "A call's saving is what the conversation would otherwise have \
     been given, less what the call returned. Otherwise is the built-in Read, which returns at most \
     25,000 tokens of a file in one call (Claude Code's documented default; a larger file counts \
     as that much, and a file over 256 KB, which a Read refuses whole, as the page the model then \
     reads; the cap is compared with cl100k_base counts until calibration lands); for a repeat \
     read's stub or delta, the answer Lumen gave the same request \
     earlier in the same conversation, which it would have sent again; for a fresh=true call \
     after a stub, and for compress_logs on inline text, nothing, so what they return is a loss.";

const ROUNDS_RULE: &str = "R is the number of turns of the same conversation (the subagent's own, \
     for a subagent's call) after the turn that read the call's result, up to the first of a \
     compaction or rewind recorded in that transcript; a collapse, inferred, where a later turn \
     sent a smaller prompt than the turn before it (context was taken out, and the result may \
     have gone with it); and the end of the session. A conversation from before the daemon \
     recorded compactions, with no collapse to see, has nothing to vouch for its end: its R may \
     be too long, so its saving is measured at R = 0 and the rest is modelled. The value of a \
     saved token is the price of putting it in context once, at the cache duration that turn \
     recorded, plus a cache read on each of the R turns at that turn's own model, speed and \
     region.";

const ROUND_COST_RULE: &str = "C, the cost of one extra round, is the turn that read the call's \
     result priced as it ran: input, cache writes, cache reads and output at its model, speed, \
     region and cache duration.";

const EXTRA_ROUNDS_RULE: &str = "A repeat-read stub or delta replaces a re-read: no extra round. \
     A recall_file or smart_read(mode=full) after an outline of the same file in the same \
     conversation, a fresh=true call after a stub of the same request, and compress_logs on \
     inline text each cost one, on evidence; so does each Read the intercept blocked, as the \
     transcript of the agent that tried it records it, priced at that agent's turn that read the \
     block. Where no transcript records the block of a path (deleted before the daemon read it, \
     or not read yet), the escape valve's faults stand for it: a block is counted once per \
     session, agent and path (the valve logs a fault on every later Read of the path too, which \
     costs nothing) and once more where the path was blocked again after smart_read or \
     recall_file answered for it. A fault that names no agent is attributed by the meter's row \
     for the same Read. Faults nothing attributes are bounded: at most one block per agent that \
     could have read the path then (the main agent, each subagent of the session with a turn \
     within five minutes of them, any agent the meter named on the path then), never more than \
     the faults; that many are counted, the first on evidence and the rest modelled. Where a \
     transcript records the block of the path, its faults are its later Reads and count as no \
     more blocks, but for agents whose transcripts were not read through their latest turn. Any \
     other call may have followed a blocked Read: where a transcript read through the call \
     records a block of the same file in the same conversation before it, which no earlier call \
     was answered by, that block is its round, counted on its own line; elsewhere one extra \
     round is counted for it and labelled modelled, and the band's high end leaves it out.";

const FLOORS_RULE: &str = "Each turn is priced at its model, speed and inference region as \
     recorded, through the app's own price table. Where a turn recorded no speed (on a model with \
     a fast mode), no region (on a model with US-only inference) or no cache-write duration, the \
     saving is priced at the lower rate and the extra round at the higher one. The band's high \
     end prices both sides the other way and leaves modelled extra rounds out; it is no bound \
     where a session may still be running or R may be too long.";

const PARTS_RULE: &str = "A dollar is the price of the model that consumed the tokens. Measured, \
     the figure shown as at least: calls priced at the turn that read them, less every extra \
     round they cost, on evidence or modelled, and the blocked Reads, each priced at the turn \
     that read the block or, where only a fault records it, that retried it; where R may be too \
     long, a saving is taken at R = 0 (the one-time price \
     alone) and a loss at R, the least either can be. Modelled, shown beside it and never added \
     to it: what those calls are worth beyond that, the cache reads over their R. Tokens only: \
     calls with no session, no turn in the ledger that read them, or a model with no price; their \
     tokens count and their dollars are not guessed from other calls'.";

/// Everything Lumen saved, from the ledger's rows. Pure: the same inputs give the same bytes.
pub fn summarize(
    ledger: &Ledger,
    clock: &Clock,
    facts: &Facts,
    cal: &dyn Calibrator,
) -> SavingsSummary {
    let chains = chain::Chains::build(ledger, clock.now_ms);
    let mut gaps = ledger.gaps.clone();
    if chains.unreadable_turns > 0 {
        gaps.push(format!(
            "{} turns have a time that could not be read and were left out",
            chains.unreadable_turns
        ));
    }

    // The blocks the transcripts record, as of the clock, in time order, so that the first of
    // each group below is its earliest; and the transcripts read for them.
    let mut recorded: Vec<&RecordedBlockRow> = ledger
        .recorded_blocks
        .iter()
        .filter(|b| b.ts_ms <= clock.now_ms)
        .collect();
    recorded.sort_by_key(|b| b.ts_ms);
    // A session, a path, and whether it was blocked again after a Lumen answer (`served`).
    type OnPath<'a> = (&'a str, &'a str, bool);
    // Of each: the agents the transcripts record a block of, and the earliest block, which the
    // faults no agent is known for are counted against.
    let mut blocked_agents: HashMap<OnPath<'_>, (HashSet<Option<&str>>, usize)> = HashMap::new();
    // Of each, by agent: the earliest block, which that agent's faults are counted against.
    let mut first_recorded: HashMap<(OnPath<'_>, Option<&str>), usize> = HashMap::new();
    for (i, b) in recorded.iter().enumerate() {
        let on = (
            b.session_id.as_str(),
            b.path.as_str(),
            b.kind == intercept::BlockKind::Served.as_str(),
        );
        let agent = b.agent_id.as_deref();
        blocked_agents
            .entry(on)
            .or_insert_with(|| (HashSet::new(), i))
            .0
            .insert(agent);
        first_recorded.entry((on, agent)).or_insert(i);
    }
    let mut recorded_faults: Vec<i64> = vec![0; recorded.len()];
    // How far each conversation's transcripts were read for blocks: its own transcript's
    // reading (`<session>.jsonl`, or the subagent's under `subagents/`), held down by any other
    // file read with lines of it. Another file never vouches for a conversation on its own.
    let mut own_read: HashMap<(&str, Option<&str>), i64> = HashMap::new();
    let mut other_read: HashMap<(&str, Option<&str>), i64> = HashMap::new();
    for r in &ledger.scanned {
        let who = (r.session_id.as_str(), r.agent_id.as_deref());
        let into = if crate::project::transcript_owner(&r.source_file) == Some(who) {
            &mut own_read
        } else {
            &mut other_read
        };
        into.entry(who)
            .and_modify(|t| *t = (*t).min(r.through_ms))
            .or_insert(r.through_ms);
    }
    let read_through = |session: &str, agent: Option<&str>| {
        let own = *own_read.get(&(session, agent))?;
        Some(
            other_read
                .get(&(session, agent))
                .map_or(own, |&other| own.min(other)),
        )
    };
    // The recorded blocks of each session, agent and path, in time order, and how many a call has
    // taken: a call that followed one is answered by it, one call per block.
    let mut blocks_on_path: HashMap<Context<'_>, (Vec<i64>, usize)> = HashMap::new();
    for b in &recorded {
        blocks_on_path
            .entry((
                b.session_id.as_str(),
                b.agent_id.as_deref(),
                b.path.as_str(),
            ))
            .or_default()
            .0
            .push(b.ts_ms);
    }

    // Every call in the order it was written, so "after an outline" means after.
    let mut outlined: HashSet<Context<'_>> = HashSet::new();
    let mut stubbed: HashSet<Context<'_>> = HashSet::new();
    let mut answers: std::collections::HashMap<Context<'_>, i64> = std::collections::HashMap::new();
    let mut drafts: Vec<Draft> = Vec::new();
    for call in &ledger.calls {
        if !CALL_ROUTES.contains(&call.routed_via.as_str()) {
            continue;
        }
        let tool = tool_of(call);
        let ts_ms = parse_ts_ms(&call.ts);
        // A summary as of a moment holds what had happened by then.
        if ts_ms.is_some_and(|t| t > clock.now_ms) {
            continue;
        }
        let key = call
            .session_id
            .as_ref()
            .map(|s| (s.as_str(), call.agent_id.as_deref(), call.path.as_str()));
        // The request, in its conversation, for the answer a repeat read would otherwise re-send.
        let request = call.session_id.as_ref().map(|s| {
            (
                s.as_str(),
                call.agent_id.as_deref(),
                call.req_key.as_deref().unwrap_or(call.path.as_str()),
            )
        });
        let after_stub = request.as_ref().is_some_and(|r| stubbed.contains(r));
        let rounds = match (
            rounds_for(call, tool, &outlined, key.as_ref(), after_stub),
            key,
            ts_ms,
        ) {
            // The block of this file this call followed is recorded, in a transcript read through
            // the call, and no earlier call took it: it is a line of its own, and this call's
            // round. A block of another file, or none recorded, leaves the round modelled.
            (Rounds::BlockNotRecorded, Some(k), Some(t))
                if read_through(k.0, k.1).is_some_and(|through| through >= t)
                    && blocks_on_path.get_mut(&k).is_some_and(|(times, taken)| {
                        let followed = times.get(*taken).is_some_and(|&b| b < t);
                        *taken += usize::from(followed);
                        followed
                    }) =>
            {
                Rounds::BlockRecorded
            }
            (r, _, _) => r,
        };
        if is_outline(tool, &call.routed_via)
            && let Some(k) = key
        {
            outlined.insert(k);
        }
        let is_repeat = REPEAT_ROUTES.contains(&call.routed_via.as_str());
        let (against, previous) = if rounds == Rounds::InlineText {
            (Against::AlreadyInContext, None)
        } else if rounds == Rounds::FreshAfterStub {
            (Against::AskedAgain, None)
        } else if is_repeat {
            match request.as_ref().and_then(|r| answers.get(r)) {
                Some(&previous) => (Against::PreviousAnswer, Some(previous)),
                None => (Against::AnswerNotInLedger, None),
            }
        } else {
            (Against::WholeFile, None)
        };
        if let Some(r) = request {
            if is_repeat {
                stubbed.insert(r);
            } else if matches!(tool, Some("smart_read" | "recall_file"))
                && call.routed_via != "smart_read_full"
            {
                // An answer a later repeat read of the same request would stand in for. Not
                // smart_read(mode="full"), which repeat reads never answer for.
                answers.insert(r, call.returned_tokens);
            }
        }
        let pair = match (&call.session_id, ts_ms) {
            (None, _) => chain::Outcome::NoPair(chain::Why::NoSession),
            (Some(_), None) => chain::Outcome::NoPair(chain::Why::UnreadableTime),
            (Some(s), Some(t)) => chains.derive(s, call.agent_id.as_deref(), t),
        };
        drafts.push(Draft {
            ts: call.ts.clone(),
            ts_ms,
            kind: LineKind::Call,
            tool,
            route: Some(call.routed_via.clone()),
            path: Some(call.path.clone()),
            recorded_cl100k: call.saved_tokens,
            against,
            full_cl100k: call.full_tokens,
            returned_cl100k: call.returned_tokens,
            previous_cl100k: previous,
            unverified: call.token_source.as_deref() != Some("measured"),
            rounds,
            pair,
            valve_faults: 0,
        });
    }

    // Blocked Reads. The escape valve logs a fault on every Read of a path the intercept had
    // blocked in that context, the first one's retry and every later Read alike, and only the
    // first stands for a round: the intercept blocks a path once per context (the session's, or
    // a subagent's own, each with its marker), and once more, with the redirect to recall_file,
    // after smart_read or recall_file has answered for it, which the fault says
    // (`after_lumen_read`). So one round per session, agent, path and kind of fault.
    //
    // Whose Read a fault was: the fault names a subagent from 1.9.0. One that names none is the
    // main agent's, or older, and the meter's row for the same Read (`builtin_read`, the same
    // session and path, written as the Read returned) says whose, where the meter recorded
    // agents by then. Those blocks are on evidence. The faults nothing attributes may still be
    // more than one agent's: a later fault on a path is an allowed Read, and a new block on it
    // can only be another agent's, which has a marker of its own. So they stand for at most one
    // block per agent that could have read the path then: the main agent, each subagent of the
    // session with a turn within [`BLOCK_WINDOW_MS`] of the faults, and any agent the meter named
    // on that path then; never more than there are faults. That many are counted, the first on
    // evidence and the rest bounded, so the cost is not understated.
    //
    // Where a transcript records the block of a path, that block is the evidence and the faults
    // are its later Reads, counted beside it and nothing more: a fault whose agent is known is
    // counted against that agent's block, and faults nothing attributes are bounded only by the
    // agents whose transcripts were not read through their latest turn, since one that was holds
    // every block of its own. A path no transcript records a block of keeps the rule above: a
    // transcript read through that holds no block of a path with faults says the reading missed
    // one as much as that none happened.
    let mut faults: Vec<(Option<i64>, usize, &BlockedReadRow)> = ledger
        .blocked_reads
        .iter()
        .enumerate()
        .map(|(n, b)| (parse_ts_ms(&b.ts), n, b))
        .filter(|(t, _, _)| !t.is_some_and(|t| t > clock.now_ms))
        .collect();
    faults.sort_by_key(|(t, n, _)| (t.is_none(), *t, *n));
    // The meter's Reads, by session and path: (time, agent, taken by a fault already).
    type MeterReads<'a> = Vec<(i64, Option<&'a str>, bool)>;
    let mut reads: std::collections::HashMap<(&str, &str), MeterReads<'_>> =
        std::collections::HashMap::new();
    for r in &ledger.valve_reads {
        if let Some(t) = r.ts_ms {
            reads
                .entry((r.session_id.as_str(), r.path.as_str()))
                .or_default()
                .push((t, r.agent_id.as_deref(), false));
        }
    }
    struct Block<'a> {
        ts_ms: Option<i64>,
        row: &'a BlockedReadRow,
        faults: i64,
        /// Its agent is known, or it is the first block of its session, path and kind; else it
        /// is one of the most there could have been.
        evidenced: bool,
    }
    type Group<'a> = (Option<&'a str>, Option<&'a str>, bool);
    /// A group's faults no evidence attributes, and the agents that are attributed.
    #[derive(Default)]
    struct Unattributed<'a> {
        faults: Vec<(Option<i64>, &'a BlockedReadRow)>,
        known: HashSet<Option<&'a str>>,
        /// The group's first block on evidence, which takes faults that stand for no more.
        first_block: Option<usize>,
    }
    let mut blocks: Vec<Block<'_>> = Vec::new();
    let mut block_of: std::collections::HashMap<(Group<'_>, Option<&str>), usize> =
        std::collections::HashMap::new();
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut unattributed: std::collections::HashMap<Group<'_>, Unattributed<'_>> =
        std::collections::HashMap::new();
    let mut recent_valve_faults = 0i64;
    let recent_from = clock.now_ms - RECENT_DAYS * DAY_MS;
    for (ts_ms, _, b) in faults {
        if ts_ms.is_some_and(|t| t >= recent_from) {
            recent_valve_faults += 1;
        }
        let again = b.detail.as_deref() == Some(AFTER_LUMEN_READ);
        let group = (b.session_id.as_deref(), b.path.as_deref(), again);
        // `Some(agent)`, `None` for the main agent; unattributed is the outer `None`.
        let agent: Option<Option<&str>> = match (&b.agent_id, group.0, group.1, ts_ms) {
            (Some(a), _, _, _) => Some(Some(a.as_str())),
            // A hook that names every subagent named none: the main agent's.
            (None, _, _, _) if names_its_agent(b.version.as_deref()) => Some(None),
            (None, Some(s), Some(p), Some(t)) => reads.get_mut(&(s, p)).and_then(|v| {
                // The meter writes its row as the Read returns, in the second of the fault or
                // just after; the nearest one no other fault has taken.
                let i = v
                    .iter()
                    .enumerate()
                    .filter(|(_, (rt, _, taken))| !taken && (t - 1000..=t + 3000).contains(rt))
                    .min_by_key(|(_, (rt, _, _))| (rt - t).abs())
                    .map(|(i, _)| i)?;
                v[i].2 = true;
                let (rt, ra, _) = v[i];
                match ra {
                    Some(a) => Some(Some(a)),
                    None if ledger.meter_agents_from.is_some_and(|f| rt >= f) => Some(None),
                    None => None,
                }
            }),
            _ => None,
        };
        let u = unattributed.entry(group).or_insert_with(|| {
            groups.push(group);
            Unattributed::default()
        });
        // The block a transcript records for this agent: the fault is a later Read of it.
        let recorded_block = match (agent, group.0, group.1) {
            (Some(a), Some(s), Some(p)) => first_recorded.get(&((s, p, again), a)).copied(),
            _ => None,
        };
        match (agent, recorded_block) {
            (Some(a), Some(i)) => {
                u.known.insert(a);
                recorded_faults[i] += 1;
            }
            (Some(a), None) => {
                u.known.insert(a);
                match block_of.get(&(group, a)) {
                    Some(&i) => blocks[i].faults += 1,
                    None => {
                        block_of.insert((group, a), blocks.len());
                        u.first_block.get_or_insert(blocks.len());
                        blocks.push(Block {
                            ts_ms,
                            row: b,
                            faults: 1,
                            evidenced: true,
                        });
                    }
                }
            }
            (None, _) => u.faults.push((ts_ms, b)),
        }
    }
    // The faults nothing attributes, group by group, in the order the groups first appeared.
    for group in groups {
        let Some(u) = unattributed.remove(&group) else {
            continue;
        };
        if u.faults.is_empty() {
            continue;
        }
        // Blocks of the path the transcripts record: the faults are their later Reads, and only
        // an agent whose transcript was not read through its latest turn may hold another.
        let in_transcripts = match (group.0, group.1) {
            (Some(s), Some(p)) => blocked_agents.get(&(s, p, group.2)),
            _ => None,
        };
        // The agents that could have been blocked on the path then, beside those known already.
        let times: Vec<i64> = u.faults.iter().filter_map(|(t, _)| *t).collect();
        let candidates = match (group.0, times.iter().min(), times.iter().max()) {
            (Some(session), Some(&first), Some(&last)) => {
                let (from, to) = (first - BLOCK_WINDOW_MS, last + BLOCK_WINDOW_MS);
                let mut who: HashSet<Option<&str>> = HashSet::new();
                who.insert(None);
                who.extend(
                    chains
                        .subagents_active(session, from, to)
                        .into_iter()
                        .map(Some),
                );
                if let Some(v) = group.1.and_then(|p| reads.get(&(session, p))) {
                    who.extend(
                        v.iter()
                            .filter(|(rt, a, _)| a.is_some() && (from..=to).contains(rt))
                            .map(|(_, a, _)| *a),
                    );
                }
                who.retain(|a| !u.known.contains(a));
                if let Some((agents, _)) = in_transcripts {
                    who.retain(|a| {
                        !agents.contains(a)
                            && !read_through(session, *a).is_some_and(|through| {
                                chains
                                    .last_turn_ms(session, *a)
                                    .is_none_or(|last| through >= last)
                            })
                    });
                }
                who.len()
            }
            // No session or no time to look agents up by: one per fault.
            _ => u.faults.len(),
        };
        let first_is_evidence = u.known.is_empty() && in_transcripts.is_none();
        let n = u
            .faults
            .len()
            .min(candidates.max(usize::from(first_is_evidence)));
        if n == 0 {
            // Every agent that could have read the path is known: these are its later Reads.
            match (u.first_block, in_transcripts) {
                (Some(i), _) => blocks[i].faults += u.faults.len() as i64,
                (None, Some((_, i))) => recorded_faults[*i] += u.faults.len() as i64,
                (None, None) => {}
            }
            continue;
        }
        // Each block at a fault of its own; the first takes the faults left over.
        let extra = u.faults.len() - n;
        for (k, (ts_ms, b)) in u.faults.into_iter().take(n).enumerate() {
            blocks.push(Block {
                ts_ms,
                row: b,
                faults: 1 + if k == 0 { extra as i64 } else { 0 },
                evidenced: k == 0 && first_is_evidence,
            });
        }
    }
    for block in blocks {
        let (ts_ms, b) = (block.ts_ms, block.row);
        let pair = match (&b.session_id, ts_ms) {
            (None, _) => chain::Outcome::NoPair(chain::Why::NoSession),
            (Some(_), None) => chain::Outcome::NoPair(chain::Why::UnreadableTime),
            (Some(s), Some(t)) => chains.retried_at(s, t),
        };
        drafts.push(Draft {
            ts: b.ts.clone(),
            ts_ms,
            kind: LineKind::BlockedRead,
            tool: None,
            route: None,
            path: b.path.clone(),
            recorded_cl100k: 0,
            against: Against::AlreadyInContext,
            full_cl100k: 0,
            returned_cl100k: 0,
            previous_cl100k: None,
            unverified: false,
            rounds: if block.evidenced {
                Rounds::BlockedRead
            } else {
                Rounds::BlockedReadBounded
            },
            pair,
            valve_faults: block.faults,
        });
    }
    // The blocks the transcripts record, each one round on evidence, priced at the turn of the
    // blocked agent that read the block's message.
    for (b, faults) in recorded.iter().zip(recorded_faults) {
        drafts.push(Draft {
            ts: iso_ms(b.ts_ms),
            ts_ms: Some(b.ts_ms),
            kind: LineKind::BlockedRead,
            tool: None,
            route: None,
            path: Some(b.path.clone()),
            recorded_cl100k: 0,
            against: Against::AlreadyInContext,
            full_cl100k: 0,
            returned_cl100k: 0,
            previous_cl100k: None,
            unverified: false,
            rounds: Rounds::BlockedReadRecorded,
            pair: chains.blocked_at(&b.session_id, b.agent_id.as_deref(), b.ts_ms),
            valve_faults: faults,
        });
    }

    let priced: Vec<Priced> = drafts
        .into_iter()
        .map(|d| price(d, cal, clock.week_start_ms))
        .collect();

    let as_of = iso_ms(clock.now_ms);
    let mut week = Acc::default();
    let mut all = Acc::default();
    for p in &priced {
        all.add(p);
        if p.line.in_week {
            week.add(p);
        }
    }

    let recent: Vec<&Priced> = priced
        .iter()
        .filter(|p| p.line.kind == LineKind::Call && p.ts_ms.is_some_and(|t| t >= recent_from))
        .collect();
    let all_calls = all.counts.calls;
    let first_ms = all.first;
    let week_period = finite_only(week.period(Some(iso_ms(clock.week_start_ms)), &as_of, &gaps));
    let all_period = finite_only(all.period(first_ms.map(iso_ms), &as_of, &gaps));

    let identity = identity_reaches_server(ledger, facts);
    let blocked: Vec<&Priced> = priced
        .iter()
        .filter(|p| {
            p.line.kind == LineKind::BlockedRead && p.ts_ms.is_some_and(|t| t >= recent_from)
        })
        .collect();
    let (status, recommendation) = judge(
        all_calls,
        &recent,
        ledger.recent_turns,
        Blocks {
            evidenced: blocked
                .iter()
                .filter(|p| p.line.extra_round_evidenced)
                .count() as i64,
            bounded: blocked
                .iter()
                .filter(|p| !p.line.extra_round_evidenced)
                .count() as i64,
            faults: recent_valve_faults,
            cost: blocked
                .iter()
                .filter_map(|p| p.line.extra_round_usd)
                .map(|c| c.value)
                .sum::<f64>(),
            priced: blocked.iter().all(|p| p.line.extra_round_usd.is_some()),
        },
        &week_period,
        &priced,
        identity,
    );

    SavingsSummary {
        schema: SCHEMA,
        as_of,
        billing_mode: facts.billing_mode,
        week: week_period,
        all_time: all_period,
        status,
        recommendation,
        method: Method {
            prices_as_of: crate::rates::PRICES_AS_OF,
            calibration: cal.info(),
            week: format!(
                "the ISO week: Monday {} 00:00 local time to now",
                clock.week_start
            ),
            counterfactual: COUNTERFACTUAL_RULE,
            rounds: ROUNDS_RULE,
            round_cost: ROUND_COST_RULE,
            extra_rounds: EXTRA_ROUNDS_RULE,
            floors: FLOORS_RULE,
            parts: PARTS_RULE,
        },
        ledger: call_ledger(priced),
    }
}

/// Why a period has no dollar figure where one did not come out a number.
const NOT_A_NUMBER: &str = "a dollar figure that came out as no number is left out";

/// A period with no figure that is not a number: a dollar part with one is left out, the period
/// is said to be partial for it, and a share that is not one is none. No surface prints `NaN`,
/// and the app is never sent one as a figure (JSON has none; it would arrive as null).
fn finite_only(mut p: Period) -> Period {
    let band_ok = |b: &Band<f64>| b.low.is_finite() && b.value.is_finite() && b.high.is_finite();
    let mut dropped = false;
    if let Some(d) = p.measured
        && !(band_ok(&d.gross) && band_ok(&d.extra_rounds) && band_ok(&d.net))
    {
        p.measured = None;
        dropped = true;
    }
    if let Some(m) = p.modelled
        && !band_ok(&m.value)
    {
        p.modelled = None;
        dropped = true;
    }
    if p.share_pct.is_some_and(|x| !x.is_finite()) {
        p.share_pct = None;
    }
    if dropped {
        p.data_quality = match p.data_quality {
            DataQuality::Partial { reason } => DataQuality::Partial {
                reason: format!("{reason}; {NOT_A_NUMBER}"),
            },
            _ => DataQuality::Partial {
                reason: NOT_A_NUMBER.to_string(),
            },
        };
    }
    p
}

/// Whether Lumen's identity hook reaches the server. The ledger's evidence wins, as in
/// `lumen doctor`: among the newest calls that carry a repeat plan, any identified one means it
/// does, and all of them unidentified means it does not. With none, the settings decide.
fn identity_reaches_server(ledger: &Ledger, facts: &Facts) -> Option<bool> {
    let newest_first = ledger
        .calls
        .iter()
        .rev()
        .map(|c| (c.routed_via.as_str(), c.repeat_plan.as_deref()));
    crate::identity_hook::reaches_server(
        facts.identity_hook_registered,
        Some(crate::identity_hook::tally(newest_first)),
    )
}

fn usd_text(x: f64) -> String {
    text::usd_signed(x)
}

/// The Reads the intercept blocked in the last [`RECENT_DAYS`] days, and what they cost.
struct Blocks {
    /// Blocks on evidence: each a transcript records, and where none does, one per session,
    /// agent, path and kind of fault.
    evidenced: i64,
    /// Blocks counted because faults no evidence attributes could stand for them: at most one
    /// per agent that could have read the path then.
    bounded: i64,
    /// The escape-valve faults behind them.
    faults: i64,
    /// The extra rounds, central, where priced.
    cost: f64,
    /// Every one of them priced.
    priced: bool,
}

/// Status and the one recommendation, from evidence only.
fn judge(
    all_calls: i64,
    recent: &[&Priced],
    recent_turns: i64,
    blocks: Blocks,
    week: &Period,
    priced: &[Priced],
    identity: Option<bool>,
) -> (Status, Option<Recommendation>) {
    let days = RECENT_DAYS;
    // The intercept blocked Reads and no Lumen tool answered any of them: every block was a round
    // spent for nothing, which is the one thing worse than no saving.
    if recent.is_empty() && blocks.evidenced + blocks.bounded > 0 {
        let cost = if blocks.cost > 0.0 {
            format!(
                ", {} {} in extra rounds",
                match (blocks.bounded > 0, blocks.priced) {
                    (true, _) => "up to",
                    (false, true) => "about",
                    (false, false) => "at least",
                },
                usd_text(blocks.cost)
            )
        } else {
            String::new()
        };
        let more = if blocks.bounded > 0 {
            format!(
                ", and up to {} more where no agent is known",
                blocks.bounded
            )
        } else {
            String::new()
        };
        return (
            Status::NotWorking {
                reason: format!(
                    "in the last {days} days the Read intercept blocked {} reads{more} ({} \
                     escape-valve faults), and no Lumen tool was called: the blocks saved \
                     nothing{cost}",
                    blocks.evidenced, blocks.faults
                ),
            },
            Some(Recommendation {
                code: "blocksWithoutSavings",
                text: format!(
                    "The Read intercept is blocking Reads that are then let through, so each \
                     block costs a round and saves nothing{cost}. Run `lumen doctor` to see \
                     whether Claude Code reaches Lumen's MCP server; until it does, \
                     LUMEN_HOOK_ENABLED=0 in the env block of ~/.claude/settings.json turns the \
                     intercept off."
                ),
            }),
        );
    }
    if recent.is_empty() {
        if recent_turns == 0 {
            return (Status::NoData, None);
        }
        let reason = if all_calls == 0 {
            format!(
                "Claude Code ran {recent_turns} rounds in the last {days} days and no Lumen tool \
                 has ever been called"
            )
        } else {
            format!(
                "Claude Code ran {recent_turns} rounds in the last {days} days and no Lumen tool \
                 was called"
            )
        };
        return (
            Status::NotWorking { reason },
            Some(Recommendation {
                code: "noCalls",
                text: "Run `lumen doctor`: it says whether Lumen's MCP server and hooks are \
                       registered with Claude Code."
                    .to_string(),
            }),
        );
    }

    // Net this week, in measured dollars where there are any, else in tokens; and whether the
    // loss is certain. The figure is a floor: a negative one whose band reaches above zero, or
    // whose high end is no bound (an R that is a lower bound or may be too long), says only that
    // Lumen may have cost more than it saved.
    let negative: Option<(String, bool)> = match week.measured {
        Some(d) => (d.net.value < 0.0).then(|| {
            (
                format!("{} this week", text::usd_phrase(d.net.value)),
                d.net.high <= 0.0 && !d.high_open,
            )
        }),
        None => (week.tokens_saved.value < 0).then(|| {
            (
                format!("{} this week", text::tokens_phrase(week.tokens_saved.value)),
                week.tokens_saved.high <= 0,
            )
        }),
    };
    let status = match &negative {
        Some((what, true)) => Status::NotWorking {
            reason: format!("Lumen cost more than it saved: {what}"),
        },
        Some((what, false)) => Status::NotWorking {
            reason: format!("Lumen may have cost more than it saved: {what}"),
        },
        None => Status::Working,
    };

    let unplaced = recent
        .iter()
        .filter(|p| p.line.pair.basis == "noSession")
        .count();
    let recommendation = if identity == Some(false) {
        Some(Recommendation {
            code: "identityHook",
            text: "Lumen cannot tell which conversation its calls come from, so repeat reads \
                   are off and its calls carry tokens and no dollars. Run Setup in the Lumen \
                   app, or see `lumen doctor`."
                .to_string(),
        })
    } else if unplaced * 2 > recent.len() {
        Some(Recommendation {
            code: "unplacedCalls",
            text: format!(
                "{unplaced} of the {} Lumen calls in the last {days} days were not tied to a \
                 conversation, so they carry tokens and no dollars. `lumen doctor` says why.",
                recent.len()
            ),
        })
    } else if negative.is_some() {
        Some(losing_week(priced))
    } else {
        None
    };
    (status, recommendation)
}

/// Why this week lost, from its losing calls with pairs of their own.
fn losing_week(priced: &[Priced]) -> Recommendation {
    let mut cut_short: Vec<i64> = Vec::new();
    let mut losing = 0usize;
    for p in priced {
        let loses = p.line.in_week
            && p.part == Part::Measured
            && p.line.kind == LineKind::Call
            && p.line.net_usd.is_some_and(|n| n.value < 0.0);
        if !loses {
            continue;
        }
        losing += 1;
        if matches!(
            p.line.pair.rounds_bound,
            Some("compaction" | "rewind" | "collapse")
        ) && let Some(r) = p.line.pair.rounds
        {
            cut_short.push(r);
        }
    }
    if losing > 0 && cut_short.len() * 2 >= losing {
        cut_short.sort_unstable();
        let median = cut_short[(cut_short.len() - 1) / 2];
        return Recommendation {
            code: "compactsEarly",
            text: format!(
                "{} of this week's {losing} losing calls were followed by a compaction or a \
                 rewind (recorded, or seen as the prompt shrinking) within a median of {median} \
                 rounds, before the tokens they saved had \
                 paid for the extra round. Compacting later lets each saved token pay for \
                 longer.",
                cut_short.len()
            ),
        };
    }
    Recommendation {
        code: "smallSavings",
        text: "This week's losing calls saved fewer tokens than the extra round each cost. \
               LUMEN_LINE_THRESHOLD (300 lines by default) is the size from which a Read is sent \
               to Lumen; a higher one sends only larger files."
            .to_string(),
    }
}

fn call_ledger(priced: Vec<Priced>) -> CallLedger {
    let mut lines: Vec<(Option<i64>, usize, CallLine)> = priced
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.ts_ms, i, p.line))
        .collect();
    // Newest first; a line whose time cannot be read sorts last; ties by the order written.
    lines.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    let any_priced = lines.iter().any(|l| l.2.net_usd.is_some());
    let score = |l: &CallLine| -> Option<f64> {
        if any_priced {
            l.net_usd.map(|n| n.value)
        } else {
            Some(l.tokens_saved.value as f64)
        }
    };
    let mut ranked: Vec<(f64, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| score(&l.2).map(|s| (s, i)))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let top_gains: Vec<CallLine> = ranked
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(TOP_N)
        .map(|(_, i)| lines[*i].2.clone())
        .collect();
    let top_losses: Vec<CallLine> = ranked
        .iter()
        .rev()
        .filter(|(s, _)| *s < 0.0)
        .take(TOP_N)
        .map(|(_, i)| lines[*i].2.clone())
        .collect();

    let omitted = lines.len().saturating_sub(LEDGER_LIMIT) as i64;
    lines.truncate(LEDGER_LIMIT);
    CallLedger {
        recent: lines.into_iter().map(|l| l.2).collect(),
        omitted,
        top_gains,
        top_losses,
    }
}
