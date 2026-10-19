//! Scoring any version's raw outputs with today's savings engine and calibration.
//!
//! An old version's own arithmetic is never used. What goes in is what was measured: the file's
//! token count and the count of what the version returned, both re-counted with today's cl100k,
//! and the route it answered on. For each flow this module builds a [`lumen_core::savings::Ledger`]
//! (the calls, plus the turns of the MODELLED reference session in `reference_session.toml`),
//! runs [`summarize`] with [`Calibrated`], and reads the engine's own lines. Nothing here
//! computes a saving, a price or a round: if the engine changes, so do these figures.
//!
//! **No self-reported savings.** [`Counts`] has two fields, `full_tokens` and `returned_tokens`.
//! There is nowhere to put a version's own `saved_tokens` (1.0.0's is clamped at zero), so it
//! cannot reach a score; the signed saving is whatever the engine makes of the two counts.
//!
//! **Flows**, each a session of its own (the engine keeps 200 lines per summary):
//! - [`Flow::Outline`]: one outline;
//! - [`Flow::OutlineThenRecall`]: the engine charges the recall's follow-up round, on evidence;
//! - [`Flow::OutlineThenStub`]: a repeat read answered with a stub;
//! - [`Flow::EditThenDelta`]: an outline, an edit, and the repeat read answered with a delta;
//! - [`Flow::BlockedThenOutline`]: for a file of [`BLOCK_MIN_LINES`] lines or more, a Read the
//!   intercept blocked and the `smart_read` that followed it. A [`RecordedBlockRow`] puts the
//!   block's round on evidence (a line of its own; the call then carries none).
//!
//! **The reference sessions** are MODELLED: the primary is lumen-core's measured context and
//! output tokens per turn (`econ::DEFAULT_*`, read, not copied), a 60k-token prompt with 400
//! output tokens is a labelled small-prompt sensitivity, and Opus 5.5 and Haiku 5.5 are model
//! sensitivities. Each is MODELLED and says so in every output ([`MODELLED`]). All the
//! calls of a flow are read by one turn, and `R` turns follow it, so `R` is exactly the number
//! of rounds remaining the engine prices for every call of the flow. The session closed more
//! than an hour before the engine's clock and the engine's `events_from` is earlier than its
//! start, so `R` is not a lower bound and not unrecorded. `R` is a parameter: [`Rounds::Floor`]
//! is 0, [`Rounds::Measured`] is the median of the no-Lumen E2E arm once it has run (until then
//! there is none and the table says so).
//!
//! **Dollars.** A model with no price in `rates.rs` (Haiku 5.5 today) gets [`Money::NoPrice`],
//! shown as "n/a — no price", never 0. The bold net is the engine's `net_usd.value`, its floor.

use crate::util::{Ctx, Res};
use lumen_core::econ;
use lumen_core::repeat::{ROUTE_DELTA, ROUTE_UNCHANGED};
use lumen_core::savings::{
    Band, BillingView, CallLine, CallRow, Clock, Facts, Ledger, LineKind, RecordedBlockRow,
    ScannedRow, TurnRow, iso_ms, summarize,
};
use lumen_core::savings::{Calibrated, SavingsSummary};
use serde::Deserialize;

/// Printed in every output that holds a figure scored against the reference session.
pub const MODELLED: &str = "MODELLED";

/// What a cell shows for a model the engine has no price for.
pub const NO_PRICE: &str = "n/a — no price";

/// A blocked-Read flow is for files of at least this many lines (the intercept's own bar).
pub const BLOCK_MIN_LINES: u32 = 300;

/// The most rounds remaining a scenario may ask for: the session must stay closed for more than
/// an hour before the clock, and each round is [`TURN_GAP_MS`] long.
pub const MAX_R: u32 = 2000;

const TURN_GAP_MS: i64 = 5_000;
const HOUR_MS: i64 = 3_600_000;
/// Monday 2026-10-12 00:00 UTC; the clock is Wednesday 2026-10-14 noon, inside the same ISO week.
const WEEK_START_MS: i64 = 1_791_763_200_000;
const NOW_MS: i64 = WEEK_START_MS + 2 * 24 * HOUR_MS + 12 * HOUR_MS;
/// The session starts six hours before the clock: `MAX_R` rounds later it is still closed for
/// more than an hour.
const SESSION_START_MS: i64 = NOW_MS - 6 * HOUR_MS;
/// The daemon recorded events from a day before the session began, so its end is on record.
const EVENTS_FROM_MS: i64 = SESSION_START_MS - 24 * HOUR_MS;
const BLOCK_MS: i64 = SESSION_START_MS;
const BLOCK_READ_TURN_MS: i64 = SESSION_START_MS + TURN_GAP_MS;
const FIRST_CALL_MS: i64 = SESSION_START_MS + 20_000;

// --- the reference session -------------------------------------------------------------------

/// `benchmarks/versions/reference_session.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSession {
    pub label: String,
    pub prices_as_of: String,
    /// The consuming model.
    pub model: String,
    /// Model sensitivities, at the primary sizes.
    pub sensitivity: Vec<String>,
    pub uncached_input_tokens: i64,
    pub small_prompt: SmallPrompt,
}

/// The small-prompt sensitivity's sizes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmallPrompt {
    pub prompt_tokens: i64,
    pub output_tokens: i64,
}

/// One session's sizes: what a turn of it consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizing {
    /// `primary` or `small prompt`.
    pub name: &'static str,
    pub prompt_tokens: i64,
    pub uncached_input_tokens: i64,
    /// Written to the cache this turn, at the 5-minute duration.
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
}

impl Sizing {
    /// The part of the prompt read from the cache.
    pub fn cache_read_tokens(&self) -> i64 {
        self.prompt_tokens - self.uncached_input_tokens - self.cache_write_tokens
    }
}

/// A column of the report: a model at a sizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub sizing: Sizing,
    pub model: String,
}

impl Column {
    pub fn label(&self) -> String {
        format!("{} prompt, {}", self.sizing.name, self.model)
    }
}

impl ReferenceSession {
    pub fn parse(text: &str) -> Res<Self> {
        let s: ReferenceSession = toml::from_str(text).ctx("reference_session.toml")?;
        s.check()?;
        Ok(s)
    }

    /// The committed file, read at build time: what a run scores against cannot be swapped by a
    /// file on disk.
    pub fn committed() -> Res<Self> {
        Self::parse(include_str!("../../versions/reference_session.toml"))
    }

    fn check(&self) -> Res<()> {
        if self.label != MODELLED {
            return Err(format!(
                "reference_session.toml: label must be {MODELLED:?}, not {:?}",
                self.label
            ));
        }
        if self.prices_as_of != lumen_core::rates::PRICES_AS_OF {
            return Err(format!(
                "reference_session.toml prices_as_of {} is not the engine's {}",
                self.prices_as_of,
                lumen_core::rates::PRICES_AS_OF
            ));
        }
        for z in [self.primary(), self.small()] {
            if z.cache_read_tokens() < 0 || z.output_tokens < 0 {
                return Err(format!(
                    "reference_session.toml: the {} prompt is smaller than its uncached and cache-write parts",
                    z.name
                ));
            }
        }
        Ok(())
    }

    fn sizing(&self, name: &'static str, prompt: i64, output: i64) -> Sizing {
        Sizing {
            name,
            prompt_tokens: prompt,
            uncached_input_tokens: self.uncached_input_tokens,
            // The write per round is the prior round's output: modelled here; econ.rs has no write term.
            cache_write_tokens: output,
            output_tokens: output,
        }
    }

    /// The primary session: lumen-core's measured context and output tokens per turn.
    pub fn primary(&self) -> Sizing {
        self.sizing(
            "primary",
            econ::DEFAULT_CONTEXT_TOKENS.round() as i64,
            econ::DEFAULT_OUTPUT_TOKENS.round() as i64,
        )
    }

    /// The small-prompt sensitivity.
    pub fn small(&self) -> Sizing {
        self.sizing(
            "small prompt",
            self.small_prompt.prompt_tokens,
            self.small_prompt.output_tokens,
        )
    }

    /// The report's columns, in a fixed order: the primary at the reference model and at each
    /// model sensitivity, then the small prompt at the reference model.
    pub fn columns(&self) -> Vec<Column> {
        let col = |sizing: Sizing, model: &str| Column {
            sizing,
            model: model.to_string(),
        };
        let mut v = vec![col(self.primary(), &self.model)];
        v.extend(self.sensitivity.iter().map(|m| col(self.primary(), m)));
        v.push(col(self.small(), &self.model));
        v
    }
}

/// Rounds remaining, R.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounds {
    /// R = 0: only the one-time price of a saved token. The floor.
    Floor,
    /// R from the no-Lumen E2E arm (its median), a whole number of rounds.
    Measured(u32),
}

impl Rounds {
    pub fn r(self) -> u32 {
        match self {
            Rounds::Floor => 0,
            Rounds::Measured(r) => r,
        }
    }

    pub fn label(self) -> String {
        match self {
            Rounds::Floor => "R=0 (floor)".to_string(),
            Rounds::Measured(r) => format!("R={r} (no-Lumen E2E median)"),
        }
    }
}

/// The two R values of the method: the floor, and the measured one when there is one.
pub fn rounds_for_report(measured: Option<u32>) -> Vec<Rounds> {
    let mut v = vec![Rounds::Floor];
    v.extend(measured.map(Rounds::Measured));
    v
}

// --- inputs ----------------------------------------------------------------------------------

/// What one reply measured, re-counted with today's cl100k. There is no field for what a version
/// said it saved: only these two counts reach the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// The file the built-in Read would have returned.
    pub full_tokens: i64,
    /// What the version returned instead.
    pub returned_tokens: i64,
}

/// One flow over one file. A loss is a `returned_tokens` larger than `full_tokens`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flow {
    Outline {
        path: String,
        outline: Counts,
    },
    OutlineThenRecall {
        path: String,
        outline: Counts,
        recall: Counts,
    },
    OutlineThenStub {
        path: String,
        outline: Counts,
        stub: Counts,
    },
    EditThenDelta {
        path: String,
        outline: Counts,
        delta: Counts,
    },
    BlockedThenOutline {
        path: String,
        file_lines: u32,
        outline: Counts,
    },
}

impl Flow {
    pub fn name(&self) -> &'static str {
        match self {
            Flow::Outline { .. } => "outline",
            Flow::OutlineThenRecall { .. } => "outline-recall",
            Flow::OutlineThenStub { .. } => "outline-stub",
            Flow::EditThenDelta { .. } => "edit-delta",
            Flow::BlockedThenOutline { .. } => "blocked-read-outline",
        }
    }

    fn path(&self) -> &str {
        match self {
            Flow::Outline { path, .. }
            | Flow::OutlineThenRecall { path, .. }
            | Flow::OutlineThenStub { path, .. }
            | Flow::EditThenDelta { path, .. }
            | Flow::BlockedThenOutline { path, .. } => path,
        }
    }

    /// The calls in the order they were made: (route, tool, counts).
    fn calls(&self) -> Vec<(&'static str, &'static str, Counts)> {
        const SMART: &str = "mcp__lumen__smart_read";
        const RECALL: &str = "mcp__lumen__recall_file";
        match self {
            Flow::Outline { outline, .. } | Flow::BlockedThenOutline { outline, .. } => {
                vec![("smart_read", SMART, *outline)]
            }
            Flow::OutlineThenRecall {
                outline, recall, ..
            } => vec![
                ("smart_read", SMART, *outline),
                ("recall_file", RECALL, *recall),
            ],
            Flow::OutlineThenStub { outline, stub, .. } => vec![
                ("smart_read", SMART, *outline),
                (ROUTE_UNCHANGED, SMART, *stub),
            ],
            Flow::EditThenDelta { outline, delta, .. } => {
                vec![
                    ("smart_read", SMART, *outline),
                    (ROUTE_DELTA, SMART, *delta),
                ]
            }
        }
    }

    fn validate(&self) -> Res<()> {
        if self.path().is_empty() {
            return Err(format!("{}: no path", self.name()));
        }
        for (_, _, c) in self.calls() {
            if c.full_tokens < 0 || c.returned_tokens < 0 {
                return Err(format!(
                    "{} {}: a token count is negative (full {}, returned {})",
                    self.name(),
                    self.path(),
                    c.full_tokens,
                    c.returned_tokens
                ));
            }
        }
        if let Flow::BlockedThenOutline { file_lines, .. } = self
            && *file_lines < BLOCK_MIN_LINES
        {
            return Err(format!(
                "{} {}: {file_lines} lines is under {BLOCK_MIN_LINES}, a file the intercept lets through",
                self.name(),
                self.path()
            ));
        }
        Ok(())
    }
}

// --- outputs ---------------------------------------------------------------------------------

/// Dollars of one line, from the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Money {
    Usd {
        value_usd: Band<f64>,
        extra_round_usd: Band<f64>,
        net_usd: Band<f64>,
    },
    /// The consuming model has no price in `rates.rs`: tokens only.
    NoPrice,
}

impl Money {
    /// The figure shown in bold: the net's floor, the engine's `net_usd.value`.
    pub fn bold_net_usd(&self) -> Option<f64> {
        match self {
            Money::Usd { net_usd, .. } => Some(net_usd.value),
            Money::NoPrice => None,
        }
    }
}

/// One call, as the engine scored it.
#[derive(Debug, Clone, PartialEq)]
pub struct CallScore {
    pub route: String,
    pub path: String,
    /// Claude's tokens (the calibration's band), signed.
    pub tokens_saved: Band<i64>,
    pub extra_rounds: i64,
    pub extra_round_basis: &'static str,
    pub extra_round_evidenced: bool,
    pub money: Money,
}

/// The blocked Read of a [`Flow::BlockedThenOutline`]: no saving, only the round it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockScore {
    pub extra_round_basis: &'static str,
    pub extra_round_evidenced: bool,
    pub extra_round_usd: Option<Band<f64>>,
}

/// A whole flow: the engine's own totals, which include a block's round.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowScore {
    pub flow: &'static str,
    pub path: String,
    pub sizing: &'static str,
    pub model: String,
    pub rounds: Rounds,
    pub calls: Vec<CallScore>,
    pub block: Option<BlockScore>,
    pub total_tokens_saved: Band<i64>,
    pub total: Money,
}

impl FlowScore {
    /// One line, labelled MODELLED. Deterministic: fixed precision, fixed order.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{MODELLED} | {} | {} prompt | {} | {} | {}\n",
            self.flow,
            self.sizing,
            self.model,
            self.rounds.label(),
            self.path
        );
        for c in &self.calls {
            out.push_str(&format!(
                "  {} | tokens {} | rounds {} ({}{}) | {}\n",
                c.route,
                band_i(&c.tokens_saved),
                c.extra_rounds,
                c.extra_round_basis,
                if c.extra_round_evidenced {
                    ", evidenced"
                } else {
                    ", modelled"
                },
                money(&c.money)
            ));
        }
        if let Some(b) = &self.block {
            out.push_str(&format!(
                "  blocked Read | round {} ({}{})\n",
                b.extra_round_usd
                    .as_ref()
                    .map_or_else(|| NO_PRICE.to_string(), band_f),
                b.extra_round_basis,
                if b.extra_round_evidenced {
                    ", evidenced"
                } else {
                    ", modelled"
                },
            ));
        }
        out.push_str(&format!(
            "  total | tokens {} | {}\n",
            band_i(&self.total_tokens_saved),
            money(&self.total)
        ));
        out
    }
}

fn band_i(b: &Band<i64>) -> String {
    format!("{} [{}, {}]", b.value, b.low, b.high)
}

fn band_f(b: &Band<f64>) -> String {
    format!("{:+.6} [{:+.6}, {:+.6}]", b.value, b.low, b.high)
}

fn money(m: &Money) -> String {
    match m {
        Money::Usd {
            value_usd,
            extra_round_usd,
            net_usd,
        } => format!(
            "value {} | extra round {} | net {} (bold {:+.6})",
            band_f(value_usd),
            band_f(extra_round_usd),
            band_f(net_usd),
            net_usd.value
        ),
        Money::NoPrice => format!("dollars {NO_PRICE}"),
    }
}

/// Every flow, at every R, for the reference model and each sensitivity model, in a fixed order
/// (flow, then model, then R), as one text. Says MODELLED on every block, and says so when there
/// is no measured R yet.
pub fn render_all(
    flows: &[Flow],
    session: &ReferenceSession,
    measured_r: Option<u32>,
) -> Res<String> {
    let mut out = format!(
        "{MODELLED}: reference sessions, prices as of {}\n",
        session.prices_as_of
    );
    for z in [session.primary(), session.small()] {
        out.push_str(&format!(
            "  {}: prompt {} tokens ({} uncached, {} written, {} read), {} output tokens\n",
            z.name,
            z.prompt_tokens,
            z.uncached_input_tokens,
            z.cache_write_tokens,
            z.cache_read_tokens(),
            z.output_tokens
        ));
    }
    if measured_r.is_none() {
        out.push_str(
            "R from the no-Lumen E2E arm: not available yet, only the R=0 floor is shown\n",
        );
    }
    for flow in flows {
        for col in session.columns() {
            for r in rounds_for_report(measured_r) {
                out.push_str(&score_flow(flow, &col.sizing, &col.model, r)?.render());
            }
        }
    }
    Ok(out)
}

// --- scoring ---------------------------------------------------------------------------------

/// Scores one flow with the engine: `model` consumes the replies, `rounds` is R.
pub fn score_flow(flow: &Flow, sizing: &Sizing, model: &str, rounds: Rounds) -> Res<FlowScore> {
    flow.validate()?;
    if rounds.r() > MAX_R {
        return Err(format!("R = {} is over the supported {MAX_R}", rounds.r()));
    }
    let id = format!(
        "lumen-bench-score-{}-{}-{}-r{}",
        flow.name(),
        sizing.name.replace(' ', "-"),
        model,
        rounds.r()
    );
    let source = format!("/bench/.claude/projects/-bench/{id}.jsonl");
    let calls = flow.calls();
    let blocked = matches!(flow, Flow::BlockedThenOutline { .. });

    // The calls, one second apart; the turn that reads them all follows the last by five.
    let call_ms = |k: usize| FIRST_CALL_MS + 1000 * k as i64;
    let reader_ms = call_ms(calls.len()) + TURN_GAP_MS;
    let mut ledger = Ledger {
        events_from: Some(EVENTS_FROM_MS),
        subagent_events_from: Some(EVENTS_FROM_MS),
        ..Ledger::default()
    };
    for (k, (route, tool, c)) in calls.iter().enumerate() {
        ledger.calls.push(CallRow {
            rowid: k as i64 + 1,
            ts: iso_ms(call_ms(k)),
            tool: (*tool).to_string(),
            path: flow.path().to_string(),
            routed_via: (*route).to_string(),
            full_tokens: c.full_tokens,
            returned_tokens: c.returned_tokens,
            // The engine does not read it for a figure (it recomputes from the two counts); it
            // is the re-counted difference, never a version's own number.
            saved_tokens: c.full_tokens - c.returned_tokens,
            session_id: Some(id.clone()),
            agent_id: None,
            req_key: None,
            token_source: Some("measured".to_string()),
            repeat_arm: None,
            repeat_plan: None,
        });
    }
    let turn = |n: usize, ms: i64| TurnRow {
        message_id: format!("t{n}"),
        session_id: id.clone(),
        ts: iso_ms(ms),
        model: Some(model.to_string()),
        speed: Some("standard".to_string()),
        inference_geo: Some("global".to_string()),
        input: sizing.uncached_input_tokens,
        output: sizing.output_tokens,
        cache_read: sizing.cache_read_tokens(),
        cache_write: sizing.cache_write_tokens,
        cache_5m: Some(sizing.cache_write_tokens),
        cache_1h: Some(0),
        is_subagent: false,
        source_file: Some(source.clone()),
    };
    let mut n = 0;
    if blocked {
        // The turn after the block read its message: that round is the block's cost.
        ledger.turns.push(turn(n, BLOCK_READ_TURN_MS));
        n += 1;
    }
    ledger.turns.push(turn(n, reader_ms));
    n += 1;
    for k in 0..i64::from(rounds.r()) {
        ledger
            .turns
            .push(turn(n, reader_ms + TURN_GAP_MS * (k + 1)));
        n += 1;
    }
    let last_turn_ms = reader_ms + TURN_GAP_MS * i64::from(rounds.r());
    debug_assert!(last_turn_ms < NOW_MS - HOUR_MS);
    if blocked {
        ledger.recorded_blocks.push(RecordedBlockRow {
            session_id: id.clone(),
            agent_id: None,
            path: flow.path().to_string(),
            ts_ms: BLOCK_MS,
            kind: "redirect".to_string(),
        });
        // The transcript was read through the whole session, so the block is on record.
        ledger.scanned.push(ScannedRow {
            source_file: source,
            session_id: id,
            agent_id: None,
            through_ms: last_turn_ms,
        });
    }

    let clock = Clock {
        now_ms: NOW_MS,
        week_start_ms: WEEK_START_MS,
        week_start: "2026-10-12".to_string(),
    };
    let facts = Facts {
        billing_mode: BillingView::Api,
        identity_hook_registered: Some(true),
    };
    let summary = summarize(&ledger, &clock, &facts, &Calibrated);
    read_summary(flow, sizing, model, rounds, &ledger, &summary)
}

fn money_of(line: &CallLine) -> Money {
    match (line.value_usd, line.extra_round_usd, line.net_usd) {
        (Some(value_usd), Some(extra_round_usd), Some(net_usd)) => Money::Usd {
            value_usd,
            extra_round_usd,
            net_usd,
        },
        _ => Money::NoPrice,
    }
}

fn read_summary(
    flow: &Flow,
    sizing: &Sizing,
    model: &str,
    rounds: Rounds,
    ledger: &Ledger,
    summary: &SavingsSummary,
) -> Res<FlowScore> {
    let lines = &summary.ledger.recent;
    let mut calls = Vec::new();
    for row in &ledger.calls {
        let line = lines
            .iter()
            .find(|l| l.kind == LineKind::Call && l.ts == row.ts)
            .ok_or_else(|| {
                format!(
                    "{}: the engine has no line for call {}",
                    flow.name(),
                    row.ts
                )
            })?;
        calls.push(CallScore {
            route: row.routed_via.clone(),
            path: row.path.clone(),
            tokens_saved: line.tokens_saved,
            extra_rounds: line.extra_rounds,
            extra_round_basis: line.extra_round_basis,
            extra_round_evidenced: line.extra_round_evidenced,
            money: money_of(line),
        });
    }
    let block = if matches!(flow, Flow::BlockedThenOutline { .. }) {
        // A blocked Read is no call line; the engine lists it among the losses when it is priced.
        let found = summary
            .ledger
            .top_losses
            .iter()
            .chain(&summary.ledger.top_gains)
            .find(|l| l.kind == LineKind::BlockedRead);
        match found {
            Some(l) => Some(BlockScore {
                extra_round_basis: l.extra_round_basis,
                extra_round_evidenced: l.extra_round_evidenced,
                extra_round_usd: l.extra_round_usd,
            }),
            None if summary.all_time.measured.is_none() => Some(BlockScore {
                extra_round_basis: "blockedReadRecorded",
                extra_round_evidenced: true,
                extra_round_usd: None,
            }),
            None => {
                return Err(format!(
                    "{}: the engine has no line for the block",
                    flow.name()
                ));
            }
        }
    } else {
        None
    };
    let total = match summary.all_time.measured {
        Some(d) => Money::Usd {
            value_usd: d.gross,
            extra_round_usd: d.extra_rounds,
            net_usd: d.net,
        },
        None => Money::NoPrice,
    };
    Ok(FlowScore {
        flow: flow.name(),
        path: flow.path().to_string(),
        sizing: sizing.name,
        model: model.to_string(),
        rounds,
        calls,
        block,
        total_tokens_saved: summary.all_time.tokens_saved,
        total,
    })
}
