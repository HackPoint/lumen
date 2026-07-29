//! The economics that size an outline.
//!
//! An intercepted read always costs exactly one extra round: the `Read` was blocked and
//! the model has to call a Lumen tool instead. So an outline is only worth returning if
//! the tokens it saves are worth more than that round. Setting the two equal gives the
//! minimum saving that justifies interception, and the budget follows:
//!
//! ```text
//! value of saving S  = S × (6.25 + 0.5·R) / 1e6    cache-write avoided once,
//!                                                   cache-read avoided on each of R rounds
//! cost of one round  = (C × 0.5 + O × 25) / 1e6    C = context tokens, O = output tokens
//!
//! S_min(C, R, O)     = (C × 0.5 + O × 25) / (6.25 + 0.5·R)
//! budget             = full_tokens − S_min
//! ```
//!
//! Two things fall out that were previously separate problems. The line-count threshold
//! becomes a curve rather than a constant — if the budget is below what a useful outline
//! costs, no outline pays for itself and the file should not be intercepted at all. And
//! a net-negative call becomes unrepresentable rather than something detected afterwards:
//! the 125-token file that recorded −54 could not have been trimmed under this rule
//! because its budget is negative before any rendering happens.
//!
//! Rates mirror `crate::rates` deliberately rather than importing them as f64: the
//! arithmetic here is per-million and mixing the two representations invites the kind of
//! silent unit error this module exists to prevent.

use serde::Serialize;

/// Per-million-token prices, matching `crate::rates`.
const CACHE_READ_PER_M: f64 = 0.5;
const CACHE_WRITE_PER_M: f64 = 6.25;
const OUTPUT_PER_M: f64 = 25.0;

/// Where the inputs to `S_min` came from.
///
/// Recorded on every metered row. The coefficients below are means over a specific
/// measured population, and a later change to how they are derived must be comparable
/// against rows scored under the old ones — which is impossible unless each row says
/// which it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EconSource {
    /// Derived from this installation's own `turns` table.
    Observed,
    /// The measured means below, because the ledger could not be read or had too few
    /// rows to be meaningful.
    MeasuredDefaults,
}

impl EconSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EconSource::Observed => "observed",
            EconSource::MeasuredDefaults => "measured_defaults",
        }
    }
}

/// Fewer turns than this and a per-installation mean is noise, so the defaults win.
const MIN_TURNS_FOR_OBSERVED: i64 = 200;

/// Measured means from the phase-E population, used when the ledger cannot supply them.
///
/// These are measurements, not guesses — 19,161 main-agent turns — but they are *this*
/// author's usage, and a different user's context profile will differ. That is why
/// `observed()` prefers the local ledger and why the choice is recorded per row.
pub const DEFAULT_CONTEXT_TOKENS: f64 = 362_965.0;
pub const DEFAULT_OUTPUT_TOKENS: f64 = 1_085.0;

/// Median main-agent rounds remaining after a given call.
///
/// Not the median session length (72). `R` is how many rounds still follow the call, and
/// a read is as likely to happen early as late, so the expected remainder is about half
/// the session. 65 is the measured median remainder, which is why it is close to but not
/// equal to half of 72.
pub const DEFAULT_ROUNDS_REMAINING: f64 = 65.0;

/// The inputs to a budget decision, and where they came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Econ {
    /// Context tokens re-read on the extra round.
    pub context_tokens: f64,
    /// Output tokens the extra round emits.
    pub output_tokens: f64,
    /// Rounds remaining, over which a saved token keeps paying.
    pub rounds_remaining: f64,
    pub source: EconSource,
}

impl Default for Econ {
    fn default() -> Self {
        Self {
            context_tokens: DEFAULT_CONTEXT_TOKENS,
            output_tokens: DEFAULT_OUTPUT_TOKENS,
            rounds_remaining: DEFAULT_ROUNDS_REMAINING,
            source: EconSource::MeasuredDefaults,
        }
    }
}

impl Econ {
    /// Cost in dollars of the one extra round interception forces.
    pub fn round_cost(&self) -> f64 {
        (self.context_tokens * CACHE_READ_PER_M + self.output_tokens * OUTPUT_PER_M) / 1e6
    }

    /// Dollar value of saving one token, over the rounds that remain.
    pub fn value_per_token(&self) -> f64 {
        (CACHE_WRITE_PER_M + CACHE_READ_PER_M * self.rounds_remaining) / 1e6
    }

    /// The minimum number of saved tokens that pays for the extra round.
    ///
    /// Returns `None` when `value_per_token` is zero or negative, which cannot happen
    /// with non-negative `R` but is checked rather than assumed — a division producing
    /// `inf` here would silently authorise an unbounded budget.
    pub fn s_min(&self) -> Option<f64> {
        let v = self.value_per_token();
        if v <= 0.0 || !v.is_finite() {
            return None;
        }
        let s = self.round_cost() / v;
        s.is_finite().then_some(s)
    }

    /// Tokens available for the outline: what the file costs, less what must be saved.
    ///
    /// Negative means no outline of any size pays for itself.
    pub fn budget(&self, full_tokens: usize) -> Option<i64> {
        let s_min = self.s_min()?;
        Some(full_tokens as i64 - s_min.ceil() as i64)
    }

    /// Read the local ledger for this installation's own means.
    ///
    /// `turns` is the right table: it is one row per billable request, so its
    /// `cache_read_input_tokens` mean is the context actually re-read per round and its
    /// `output_tokens` mean is `O` directly. Subagent turns are excluded — they carry
    /// their own fresh context and are not the rounds an interception adds to.
    ///
    /// `rounds_remaining` is left at the default: deriving it needs the per-call
    /// `(R, round cost)` pairing that `req_key` was added to make possible, and until
    /// that data exists a computed value here would be a guess wearing the word
    /// "observed". Reported as `Observed` only when `C` and `O` both came from the
    /// ledger.
    pub fn observed(db: &std::path::Path) -> Self {
        let fallback = Self::default();
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ) else {
            return fallback;
        };

        let row: Result<(i64, Option<f64>, Option<f64>), _> = conn.query_row(
            "SELECT COUNT(*), AVG(cache_read_input_tokens), AVG(output_tokens)
             FROM turns
             WHERE COALESCE(is_subagent, 0) = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );

        match row {
            Ok((n, Some(c), Some(o))) if n >= MIN_TURNS_FOR_OBSERVED && c > 0.0 && o > 0.0 => {
                Self {
                    context_tokens: c,
                    output_tokens: o,
                    rounds_remaining: DEFAULT_ROUNDS_REMAINING,
                    source: EconSource::Observed,
                }
            }
            _ => fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published break-even: 1,825 tokens at a 100k context and 72 rounds.
    ///
    /// This is the arithmetic the whole feature rests on, checked against a figure
    /// derived independently during phase E. If it drifts, every budget is wrong.
    #[test]
    fn s_min_reproduces_the_published_break_even() {
        let e = Econ {
            context_tokens: 100_000.0,
            output_tokens: 1_085.0,
            rounds_remaining: 72.0,
            source: EconSource::MeasuredDefaults,
        };
        let s = e.s_min().unwrap();
        assert!(
            (s - 1825.4).abs() < 0.5,
            "expected ~1825.4 saved tokens to break even at 100k/72, got {s}"
        );
    }

    /// At the measured mean context the bar is far higher than at 100k, which is the
    /// point of making the threshold a curve: the same file is worth trimming in a small
    /// context and not worth it in a large one.
    #[test]
    fn the_bar_rises_with_context() {
        let small = Econ {
            context_tokens: 50_000.0,
            ..Default::default()
        };
        let large = Econ {
            context_tokens: 400_000.0,
            ..Default::default()
        };
        let (a, b) = (small.s_min().unwrap(), large.s_min().unwrap());
        assert!(
            a < b,
            "a larger context must demand a larger saving: {a} vs {b}"
        );
        // And the default population sits where the phase-E numbers put it.
        let d = Econ::default().s_min().unwrap();
        assert!(
            (5_000.0..6_000.0).contains(&d),
            "the default S_min should be ~5.4k tokens, got {d}"
        );
    }

    /// More rounds remaining means a saved token pays more often, so the bar drops.
    #[test]
    fn the_bar_falls_as_more_rounds_remain() {
        let early = Econ {
            rounds_remaining: 200.0,
            ..Default::default()
        };
        let late = Econ {
            rounds_remaining: 2.0,
            ..Default::default()
        };
        assert!(early.s_min().unwrap() < late.s_min().unwrap());
    }

    /// The structural claim: a file too small to repay the round gets a negative budget,
    /// so there is no outline to render and the call cannot be net-negative.
    ///
    /// 125 tokens is the real file that recorded a −54 saving under the fixed-size
    /// outline.
    #[test]
    fn a_file_too_small_to_repay_the_round_has_no_budget() {
        let e = Econ::default();
        assert!(
            e.budget(125).unwrap() < 0,
            "the 125-token file that lost 54 tokens must not be interceptable"
        );
        assert!(
            e.budget(2_000).unwrap() < 0,
            "2k tokens is still below the measured bar"
        );
        assert!(
            e.budget(50_000).unwrap() > 0,
            "a genuinely large file must have room"
        );
    }

    /// Zero rounds remaining still leaves the cache-write credit, so the bar is finite.
    #[test]
    fn zero_rounds_remaining_is_finite_not_infinite() {
        let e = Econ {
            rounds_remaining: 0.0,
            ..Default::default()
        };
        let s = e
            .s_min()
            .expect("cache-write alone keeps the denominator positive");
        assert!(s.is_finite() && s > 0.0);
    }

    #[test]
    fn source_labels_are_stable_strings() {
        assert_eq!(EconSource::Observed.as_str(), "observed");
        assert_eq!(EconSource::MeasuredDefaults.as_str(), "measured_defaults");
    }

    #[test]
    fn a_missing_ledger_falls_back_to_the_measured_defaults() {
        let e = Econ::observed(std::path::Path::new("/nonexistent/lumen.db"));
        assert_eq!(e.source, EconSource::MeasuredDefaults);
        assert_eq!(e.context_tokens, DEFAULT_CONTEXT_TOKENS);
    }

    /// A ledger with too few turns must not be trusted over the measured population.
    #[test]
    fn a_nearly_empty_ledger_falls_back_rather_than_using_a_noisy_mean() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(crate::schema::DDL).unwrap();
            for i in 0..5 {
                c.execute(
                    "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,
                                       output_tokens,cache_read_input_tokens,
                                       cache_creation_input_tokens)
                     VALUES(?1,'s','2026-01-01T00:00:00Z','m',0,10,999999,0)",
                    [format!("m{i}")],
                )
                .unwrap();
            }
        }
        let e = Econ::observed(&db);
        assert_eq!(
            e.source,
            EconSource::MeasuredDefaults,
            "5 turns is not a population; 999,999 must not become the context mean"
        );
    }

    #[test]
    fn a_populated_ledger_supplies_observed_values() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(crate::schema::DDL).unwrap();
            let tx = c.unchecked_transaction().unwrap();
            for i in 0..MIN_TURNS_FOR_OBSERVED {
                tx.execute(
                    "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,
                                       output_tokens,cache_read_input_tokens,
                                       cache_creation_input_tokens)
                     VALUES(?1,'s','2026-01-01T00:00:00Z','m',0,500,120000,0)",
                    [format!("m{i}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let e = Econ::observed(&db);
        assert_eq!(e.source, EconSource::Observed);
        assert!((e.context_tokens - 120_000.0).abs() < 1.0);
        assert!((e.output_tokens - 500.0).abs() < 1.0);
        // Rounds stay at the default until per-call (R, cost) pairs exist.
        assert_eq!(e.rounds_remaining, DEFAULT_ROUNDS_REMAINING);
    }

    /// Subagent turns carry their own context and must not move the mean.
    #[test]
    fn subagent_turns_are_excluded_from_the_observed_mean() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(crate::schema::DDL).unwrap();
            let tx = c.unchecked_transaction().unwrap();
            for i in 0..MIN_TURNS_FOR_OBSERVED {
                tx.execute(
                    "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,
                                       output_tokens,cache_read_input_tokens,
                                       cache_creation_input_tokens,is_subagent)
                     VALUES(?1,'s','2026-01-01T00:00:00Z','m',0,500,100000,0,0)",
                    [format!("main{i}")],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,
                                       output_tokens,cache_read_input_tokens,
                                       cache_creation_input_tokens,is_subagent)
                     VALUES(?1,'s','2026-01-01T00:00:00Z','m',0,500,1,0,1)",
                    [format!("sub{i}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let e = Econ::observed(&db);
        assert!(
            (e.context_tokens - 100_000.0).abs() < 1.0,
            "subagent rows with a 1-token context dragged the mean to {}",
            e.context_tokens
        );
    }
}
