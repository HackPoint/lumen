//! `score.rs` against today's engine: hand-computed figures, the rounds the engine charges, the
//! absence of a price, and byte-identical output.
//!
//! The hand-computed case, worked out from `rates.rs`, `calibration.rs`, `econ.rs` and
//! `benchmarks/versions/reference_session.toml`:
//!
//! PRIMARY reference session (claude-sonnet-5-5, MODELLED): lumen-core's measured context of
//! 362,965 tokens and 1,085 output tokens per turn (`econ::DEFAULT_*`). Split as `econ.rs` models
//! a round: 10 uncached, a 5-minute cache write equal to the prior round's output (1,085), and
//! 362,965 - 10 - 1,085 = 361,870 read from the cache. Sonnet 5.5 prices per million: input 2,
//! output 10, 5-minute cache write 2.5, cache read 0.1.
//!
//! Flow: one outline of `a.rs`, full 10,000 and returned 1,004 cl100k tokens, R = 0.
//! - File (Rust, Sonnet 5.5: median 1.587, min 1.435, max 1.699):
//!   low = floor(10,000 x 1.435) = 14,350; value = 15,870; high = ceil(10,000 x 1.699) = 16,990.
//!   Under the 25,000 Read cap.
//! - Answer (the engine converts an answer at the FILE's median, 1.587, and the widest span the
//!   model measured over every type, min 1.335, max 1.794):
//!   low = floor(1,004 x 1.335) = 1,340; value = round(1,593.348) = 1,593;
//!   high = ceil(1,801.176) = 1,802.
//! - tokens saved: low = 14,350 - 1,802 = 12,548; value = 15,870 - 1,593 = 14,277;
//!   high = 16,990 - 1,340 = 15,650. (Independent of the session's size.)
//! - One-time price of a saved token: the consuming turn wrote 1,085 tokens at 5 minutes, so
//!   2.5 per million. R = 0, so no cache reads. Value in dollars takes the LOW end of the tokens
//!   (a floor): value.low = value.value = 12,548 x 2.5e-6 = $0.031370;
//!   value.high = 15,650 x 2.5e-6 = $0.039125.
//! - C, one extra round, is the consuming turn's own usage:
//!   (10 x 2 + 1,085 x 10 + 361,870 x 0.1 + 1,085 x 2.5) / 1e6
//!   = (20 + 10,850 + 36,187 + 2,712.5) / 1e6 = 49,769.5 / 1e6 = $0.0497695, a tie between two micro-dollars
//!   (the engine reports $0.049769; the tests accept within half a micro-dollar of the exact value). The outline has no recorded block, so the engine
//!   counts one modelled round (the call may have followed a blocked Read):
//!   extra round {low 0, value 0.0497695, high 0.0497695}.
//! - net = value less cost with the ends crossed: low = 0.031370 - 0.0497695 = -$0.0183995,
//!   value = -$0.0183995 (the bold figure), high = 0.039125 - 0 = $0.039125.
//! - R = 10: a saved token is also re-read from the cache on 10 later turns, 10 x 0.1 = 1.0, so
//!   3.5 per million: value.value = 12,548 x 3.5e-6 = $0.043918, net value
//!   0.043918 - 0.0497695 = -$0.0058515.
//! - Opus 5.5 (input 4, output 20, write 5, read 0.2; same ratios), R = 0: value.value =
//!   12,548 x 5e-6 = $0.062740; C = (10 x 4 + 1,085 x 20 + 361,870 x 0.2 + 1,085 x 5) / 1e6 =
//!   (40 + 21,700 + 72,374 + 5,425) / 1e6 = $0.099539; net value = -$0.036799.
//!
//! SMALL-PROMPT sensitivity (60,000 prompt = 10 + 400 written + 59,590 read; 400 output),
//! Sonnet 5.5, R = 0: C = (10 x 2 + 400 x 10 + 59,590 x 0.1 + 400 x 2.5) / 1e6
//! = (20 + 4,000 + 5,959 + 1,000) / 1e6 = $0.010979; the value is the same $0.031370, so the
//! net value is $0.020391; at R = 10 the value is $0.043918 and the net $0.032939.

use lumen_bench::score::{
    BLOCK_MIN_LINES, Counts, Flow, MODELLED, Money, NO_PRICE, ReferenceSession, Rounds, Sizing,
    render_all, score_flow,
};
use lumen_core::savings::Band;

const SONNET: &str = "claude-sonnet-5-5";
const OPUS: &str = "claude-opus-5-5";
const HAIKU: &str = "claude-haiku-5-5";

fn session() -> ReferenceSession {
    ReferenceSession::committed().unwrap()
}

fn primary() -> Sizing {
    session().primary()
}

fn small() -> Sizing {
    session().small()
}

fn counts(full: i64, returned: i64) -> Counts {
    // A new field on `Counts` (a self-reported saving) breaks this pattern: it must not exist.
    let c = Counts {
        full_tokens: full,
        returned_tokens: returned,
    };
    let Counts {
        full_tokens: _,
        returned_tokens: _,
    } = c;
    c
}

fn outline(full: i64, returned: i64) -> Flow {
    Flow::Outline {
        path: "src/a.rs".into(),
        outline: counts(full, returned),
    }
}

/// `actual` is the exact hand-computed `exact` dollars to the micro-dollar the engine reports.
/// The primary round costs $0.0497695, a tie between two micro-dollars, so the engine's rounding
/// may fall either way: it must be within half a micro-dollar (`halves` of them) of the exact value.
fn near(actual: f64, exact: f64, halves: f64) -> bool {
    (actual - exact).abs() <= halves * 0.5e-6 + 1e-12
}

/// Micro-dollars, so the comparison is exact.
fn micro(x: f64) -> i64 {
    (x * 1e6).round() as i64
}

fn micro_band(b: &Band<f64>) -> (i64, i64, i64) {
    (micro(b.low), micro(b.value), micro(b.high))
}

fn usd(m: &Money) -> (Band<f64>, Band<f64>, Band<f64>) {
    match m {
        Money::Usd {
            value_usd,
            extra_round_usd,
            net_usd,
        } => (*value_usd, *extra_round_usd, *net_usd),
        Money::NoPrice => panic!("expected dollars, got no price"),
    }
}

#[test]
fn a_hand_computed_outline_matches_to_the_micro_dollar() {
    let s = score_flow(&outline(10_000, 1_004), &primary(), SONNET, Rounds::Floor).unwrap();
    let c = &s.calls[0];
    assert_eq!(
        (
            c.tokens_saved.low,
            c.tokens_saved.value,
            c.tokens_saved.high
        ),
        (12_548, 14_277, 15_650)
    );
    let (value, extra, net) = usd(&c.money);
    assert_eq!(micro_band(&value), (31_370, 31_370, 39_125));
    assert_eq!(micro(extra.low), 0);
    assert!(
        near(extra.value, 0.049_769_5, 1.0) && extra.high == extra.value,
        "{extra:?}"
    );
    assert_eq!(micro(net.high), 39_125);
    assert!(
        near(net.low, -0.018_399_5, 1.0) && net.low == net.value,
        "{net:?}"
    );
    assert!(near(c.money.bold_net_usd().unwrap(), -0.018_399_5, 1.0));
    // The flow's total is the engine's own sum of the one line.
    assert_eq!(s.total_tokens_saved, c.tokens_saved);
    assert_eq!(usd(&s.total).2.value, net.value);

    let r10 = score_flow(
        &outline(10_000, 1_004),
        &primary(),
        SONNET,
        Rounds::Measured(10),
    )
    .unwrap();
    let (value, _, net) = usd(&r10.calls[0].money);
    assert_eq!(micro(value.value), 43_918);
    assert!(near(net.value, -0.005_851_5, 1.0), "{net:?}");

    let opus = score_flow(&outline(10_000, 1_004), &primary(), OPUS, Rounds::Floor).unwrap();
    let (value, extra, net) = usd(&opus.calls[0].money);
    assert_eq!(micro(value.value), 62_740);
    assert_eq!(micro(extra.value), 99_539);
    assert_eq!(micro(net.value), -36_799);
}

#[test]
fn the_small_prompt_sensitivity_keeps_the_60k_figures_and_is_labelled() {
    let z = small();
    assert_eq!(z.name, "small prompt");
    assert_eq!((z.prompt_tokens, z.output_tokens), (60_000, 400));
    assert_eq!((z.cache_write_tokens, z.cache_read_tokens()), (400, 59_590));
    let s = score_flow(&outline(10_000, 1_004), &z, SONNET, Rounds::Floor).unwrap();
    let (value, extra, net) = usd(&s.calls[0].money);
    assert_eq!(micro(value.value), 31_370);
    assert_eq!(micro_band(&extra), (0, 10_979, 10_979));
    assert_eq!(micro(net.value), 20_391);
    let r10 = score_flow(&outline(10_000, 1_004), &z, SONNET, Rounds::Measured(10)).unwrap();
    let (value, _, net) = usd(&r10.calls[0].money);
    assert_eq!((micro(value.value), micro(net.value)), (43_918, 32_939));
    assert!(s.render().contains("small prompt prompt"));
}

#[test]
fn the_primary_session_is_lumen_cores_measured_defaults_not_60k() {
    let z = primary();
    assert_eq!(z.name, "primary");
    assert_eq!(
        z.prompt_tokens,
        lumen_core::econ::DEFAULT_CONTEXT_TOKENS.round() as i64
    );
    assert_eq!(
        z.output_tokens,
        lumen_core::econ::DEFAULT_OUTPUT_TOKENS.round() as i64
    );
    assert_eq!(
        (
            z.uncached_input_tokens,
            z.cache_write_tokens,
            z.cache_read_tokens()
        ),
        (10, 1_085, 361_870)
    );
    // The primary is the first column, the small prompt the last.
    let cols = session().columns();
    assert_eq!(cols.first().unwrap().sizing, z);
    assert_eq!(cols.last().unwrap().sizing, small());
    assert_eq!(cols.len(), 4);
}

#[test]
fn the_bold_net_is_the_floor_not_the_central_estimate() {
    let s = score_flow(&outline(10_000, 1_004), &primary(), SONNET, Rounds::Floor).unwrap();
    let c = &s.calls[0];
    let bold = c.money.bold_net_usd().unwrap();
    let (_, _, net) = usd(&c.money);
    // At the median ratios the saving is 14,277 tokens, not 12,548: pricing those gives
    // 14,277 x 2.5e-6 - 0.0497695 = -$0.013077. The bold figure is the floor, -$0.018400.
    assert!(near(bold, -0.018_399_5, 1.0), "{bold}");
    assert!(bold < -0.013_077 + 1e-9);
    assert_eq!(bold, net.value);
    assert!(bold < net.high);
}

#[test]
fn a_recall_gets_its_follow_up_round_on_evidence() {
    let flow = Flow::OutlineThenRecall {
        path: "src/a.rs".into(),
        outline: counts(10_000, 1_004),
        recall: counts(10_000, 2_000),
    };
    let s = score_flow(&flow, &primary(), SONNET, Rounds::Floor).unwrap();
    let outline = &s.calls[0];
    let recall = &s.calls[1];
    assert_eq!(outline.extra_round_basis, "blockNotRecorded");
    assert!(!outline.extra_round_evidenced);
    assert_eq!(recall.extra_round_basis, "followUp");
    assert!(recall.extra_round_evidenced);
    assert_eq!(recall.extra_rounds, 1);
    let (_, extra, _) = usd(&recall.money);
    // On evidence the round is in the floor as well: low = value = high = C.
    assert!(
        near(extra.low, 0.049_769_5, 1.0) && extra.low == extra.value && extra.value == extra.high
    );
    // The flow pays both rounds.
    // The engine sums the two rounded lines: within a micro-dollar of 2 x 0.0497695.
    assert!(near(usd(&s.total).1.value, 0.099_539, 2.0));
}

#[test]
fn stubs_and_deltas_replace_a_reread_one_for_one() {
    for flow in [
        Flow::OutlineThenStub {
            path: "src/a.rs".into(),
            outline: counts(10_000, 1_004),
            stub: counts(10_000, 60),
        },
        Flow::EditThenDelta {
            path: "src/a.rs".into(),
            outline: counts(10_000, 1_004),
            delta: counts(10_000, 300),
        },
    ] {
        let s = score_flow(&flow, &primary(), SONNET, Rounds::Floor).unwrap();
        let repeat = &s.calls[1];
        assert_eq!(repeat.extra_round_basis, "repeatAnswer", "{}", flow.name());
        assert_eq!(repeat.extra_rounds, 0);
        // Against the earlier answer (1,004 cl100k tokens, about 1,600 of Claude's), not the
        // file (about 15,900): a small, positive saving.
        assert!(repeat.tokens_saved.value > 0);
        assert!(
            repeat.tokens_saved.value < 1_600,
            "{:?}",
            repeat.tokens_saved
        );
    }
}

#[test]
fn haiku_5_5_has_no_dollars_and_says_so() {
    let s = score_flow(&outline(10_000, 1_004), &primary(), HAIKU, Rounds::Floor).unwrap();
    assert_eq!(s.calls[0].money, Money::NoPrice);
    assert_eq!(s.total, Money::NoPrice);
    assert_eq!(s.calls[0].money.bold_net_usd(), None);
    // Tokens are still scored.
    assert!(s.calls[0].tokens_saved.value > 0);
    let text = s.render();
    assert!(text.contains(NO_PRICE), "{text}");
    assert!(text.contains(MODELLED));
    assert!(
        !text.contains("+0.000000"),
        "a missing price must not print as 0: {text}"
    );
}

#[test]
fn a_loss_stays_negative() {
    // The version returned 300 tokens for a file of 100.
    let s = score_flow(&outline(100, 300), &primary(), SONNET, Rounds::Floor).unwrap();
    let c = &s.calls[0];
    // File: floor(143.5) = 143, round(158.7) = 159, ceil(169.9) = 170. Answer: floor(400.5) =
    // 400, round(476.1) = 476 (the file's median), ceil(538.2) = 539. low = 143 - 539 = -396,
    // value = 159 - 476 = -317, high = 170 - 400 = -230.
    assert_eq!(
        (
            c.tokens_saved.low,
            c.tokens_saved.value,
            c.tokens_saved.high
        ),
        (-396, -317, -230)
    );
    let (value, _, net) = usd(&c.money);
    // A loss is priced at its largest: -396 x 2.5e-6 = -$0.000990, then the modelled round.
    assert_eq!(micro(value.value), -990);
    assert!(near(net.value, -0.000_99 - 0.049_769_5, 1.0), "{net:?}");
    assert!(c.money.bold_net_usd().unwrap() < 0.0);
}

#[test]
fn a_blocked_read_charges_its_round_on_evidence() {
    let flow = Flow::BlockedThenOutline {
        path: "src/a.rs".into(),
        file_lines: 400,
        outline: counts(10_000, 1_004),
    };
    let s = score_flow(&flow, &primary(), SONNET, Rounds::Floor).unwrap();
    let call = &s.calls[0];
    // The block is a line of its own; the call that followed it carries no round.
    assert_eq!(call.extra_round_basis, "blockRecorded");
    assert_eq!(call.extra_rounds, 0);
    assert_eq!(micro_band(&usd(&call.money).1), (0, 0, 0));
    let block = s.block.as_ref().unwrap();
    assert_eq!(block.extra_round_basis, "blockedReadRecorded");
    assert!(block.extra_round_evidenced);
    let round = block.extra_round_usd.unwrap();
    assert!(near(round.value, 0.049_769_5, 1.0) && round.low == round.high);
    // The flow's total pays the block's round once: the same -$0.018400 net, now on evidence.
    let (_, extra, net) = usd(&s.total);
    assert!(near(extra.value, 0.049_769_5, 1.0));
    assert!(near(net.value, -0.018_399_5, 1.0));
    // And a file under the intercept's bar is refused rather than scored as a block.
    let small = Flow::BlockedThenOutline {
        path: "src/a.rs".into(),
        file_lines: BLOCK_MIN_LINES - 1,
        outline: counts(10_000, 1_004),
    };
    assert!(score_flow(&small, &primary(), SONNET, Rounds::Floor).is_err());
}

#[test]
fn a_larger_r_adds_the_cache_reads_and_nothing_else() {
    let flow = outline(10_000, 1_004);
    let at = |r| {
        let s = score_flow(&flow, &primary(), SONNET, r).unwrap();
        usd(&s.calls[0].money)
    };
    let (v0, c0, _) = at(Rounds::Floor);
    let (v10, c10, _) = at(Rounds::Measured(10));
    // 12,548 saved tokens x 10 reads x $0.10 per million = $0.012548.
    assert_eq!(micro(v10.value) - micro(v0.value), 12_548);
    assert_eq!(c0, c10, "R does not change what a round costs");
    // Same tokens either way.
    let t0 = score_flow(&flow, &primary(), SONNET, Rounds::Floor).unwrap();
    let t10 = score_flow(&flow, &primary(), SONNET, Rounds::Measured(10)).unwrap();
    assert_eq!(t0.calls[0].tokens_saved, t10.calls[0].tokens_saved);
    assert!(score_flow(&flow, &primary(), SONNET, Rounds::Measured(2001)).is_err());
}

#[test]
fn the_same_inputs_give_the_same_bytes() {
    let flows = vec![
        outline(10_000, 1_004),
        Flow::OutlineThenRecall {
            path: "src/a.rs".into(),
            outline: counts(10_000, 1_004),
            recall: counts(10_000, 2_000),
        },
        Flow::BlockedThenOutline {
            path: "src/a.rs".into(),
            file_lines: 400,
            outline: counts(10_000, 1_004),
        },
    ];
    let a = render_all(&flows, &session(), Some(7)).unwrap();
    let b = render_all(&flows, &session(), Some(7)).unwrap();
    assert_eq!(a, b);
    // Every flow block is labelled, for each of the three models and both R values.
    assert_eq!(a.matches(&format!("{MODELLED} | ")).count(), 3 * 4 * 2);
    assert!(a.contains(NO_PRICE));
    assert!(a.contains("R=7 (no-Lumen E2E median)"));
    let none = render_all(&flows, &session(), None).unwrap();
    assert!(none.contains("not available yet"));
    assert!(!none.contains("R=7"));
}

#[test]
fn the_committed_reference_session_is_the_one_in_the_method() {
    let s = session();
    assert_eq!(s.model, SONNET);
    assert_eq!(s.sensitivity, [OPUS, HAIKU]);
    assert_eq!(s.prices_as_of, lumen_core::rates::PRICES_AS_OF);
    // A drifted price date, a wrong label or a prompt smaller than its parts is refused.
    let text = include_str!("../../versions/reference_session.toml");
    assert!(ReferenceSession::parse(&text.replace("2026-10-13", "2026-01-01")).is_err());
    assert!(ReferenceSession::parse(&text.replace("\"MODELLED\"", "\"measured\"")).is_err());
    assert!(ReferenceSession::parse(&text.replace("= 60000", "= 1")).is_err());
}

#[test]
fn negative_counts_are_refused() {
    assert!(score_flow(&outline(-1, 5), &primary(), SONNET, Rounds::Floor).is_err());
}
