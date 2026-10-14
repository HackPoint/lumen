//! Fixture tests for the engine: every rule in the module docs, from rows built by hand.

use super::*;
use crate::rates;
use crate::repeat::PLAN_UNIDENTIFIED;

const SONNET: &str = "claude-sonnet-4-6"; // [3, 15, 3.75, 6, 0.3]
const OPUS: &str = "claude-opus-4-8"; // [5, 25, 6.25, 10, 0.5], fast [10, 50, 12.5, 20, 1]

/// Monday 2026-10-12 00:00 UTC; the clock is Wednesday noon of that week.
const MONDAY: i64 = 1_791_763_200_000;
const NOW: i64 = MONDAY + 2 * 86_400_000 + 12 * 3_600_000;

fn ts(ms: i64) -> String {
    iso_ms(ms)
}

fn clock() -> Clock {
    Clock {
        now_ms: NOW,
        week_start_ms: MONDAY,
        week_start: "2026-10-12".into(),
    }
}

fn facts() -> Facts {
    Facts {
        billing_mode: BillingView::Api,
        identity_hook_registered: Some(true),
    }
}

/// Monday of the week, plus hours and seconds.
fn at(hours: i64, secs: i64) -> i64 {
    MONDAY + hours * 3_600_000 + secs * 1000
}

fn call(
    rowid: i64,
    session: Option<&str>,
    ms: i64,
    route: &str,
    path: &str,
    saved: i64,
) -> CallRow {
    let tool = match route {
        r if r.starts_with("recall_file") => "mcp__lumen__recall_file",
        "compress_logs" => "mcp__lumen__compress_logs",
        _ => "mcp__lumen__smart_read",
    };
    CallRow {
        rowid,
        ts: ts(ms),
        tool: tool.into(),
        path: path.into(),
        routed_via: route.into(),
        full_tokens: saved + 2_000,
        returned_tokens: 2_000,
        saved_tokens: saved,
        session_id: session.map(str::to_string),
        agent_id: None,
        req_key: None,
        token_source: Some("measured".into()),
        repeat_arm: None,
        repeat_plan: None,
    }
}

fn turn(id: &str, session: &str, ms: i64, model: &str) -> TurnRow {
    TurnRow {
        message_id: id.into(),
        session_id: session.into(),
        ts: ts(ms),
        model: Some(model.into()),
        speed: Some("standard".into()),
        inference_geo: Some("global".into()),
        input: 10,
        output: 500,
        cache_read: 50_000,
        cache_write: 3_000,
        cache_5m: Some(3_000),
        cache_1h: Some(0),
        is_subagent: false,
        source_file: Some(format!("/h/.claude/projects/-p/{session}.jsonl")),
    }
}

fn run(ledger: &Ledger) -> SavingsSummary {
    summarize(ledger, &clock(), &facts(), &Uncalibrated)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// One session: the turn that made the call, the call, the turn that read it, two more, a
/// compaction, and one after it.
fn derived_ledger() -> Ledger {
    Ledger {
        calls: vec![call(
            1,
            Some("s"),
            at(1, 5),
            "smart_read",
            "/r/a.rs",
            10_000,
        )],
        turns: vec![
            turn("t0", "s", at(1, 0), SONNET),
            turn("t1", "s", at(1, 9), SONNET),
            turn("t2", "s", at(1, 60), SONNET),
            turn("t3", "s", at(1, 120), SONNET),
            turn("t4", "s", at(1, 180), SONNET),
            turn("t5", "s", at(1, 240), SONNET),
        ],
        boundaries: vec![BoundaryRow {
            session_id: "s".into(),
            kind: "compaction".into(),
            ts_ms: at(1, 150),
            source_file: Some("/h/.claude/projects/-p/s.jsonl".into()),
        }],
        // The daemon was recording events before any of it.
        events_from: Some(0),
        subagent_events_from: Some(0),
        ..Default::default()
    }
}

#[test]
fn a_derived_pair_takes_r_and_c_from_the_turn_chain_and_a_compaction_ends_r() {
    let s = run(&derived_ledger());
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.basis, "derived");
    assert_eq!(line.pair.model.as_deref(), Some(SONNET));
    // t2 and t3 follow the turn that read it (t1) before the compaction; t4 and t5 do not count.
    assert_eq!(line.pair.rounds, Some(2));
    assert_eq!(line.pair.rounds_bound, Some("compaction"));
    assert_eq!(line.pair.ttl, Some("5m"));

    // Value: 10,000 × (3.75 written once + 0.3 read on each of 2 rounds) / 1e6.
    let value = line.value_usd.unwrap();
    assert!(
        close(value.value, 10_000.0 * (3.75 + 2.0 * 0.3) / 1e6),
        "{value:?}"
    );
    // C: t1 as it ran: 10 in × 3 + 500 out × 15 + 50,000 read × 0.3 + 3,000 written × 3.75.
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    let cost = line.extra_round_usd.unwrap();
    // A first smart_read may have followed a blocked Read: counted, modelled, out of `low`.
    assert_eq!(line.extra_round_basis, "blockNotRecorded");
    assert!(!line.extra_round_evidenced);
    assert!(
        close(cost.value, c) && close(cost.high, c) && close(cost.low, 0.0),
        "{cost:?}"
    );
    let net = line.net_usd.unwrap();
    assert!(close(net.value, value.value - c), "{net:?}");
    assert!(
        close(net.high, value.high),
        "the high end leaves the modelled round out"
    );
    assert!(net.low <= net.value && net.value <= net.high);

    assert_eq!(s.all_time.counts.derived_pairs, 1);
    assert_eq!(s.all_time.counts.extra_rounds_modelled, 1);
    // A trusted R: measured, the modelled round charged in it.
    assert_eq!(line.dollars, "measured");
    let measured = s.all_time.measured.unwrap();
    assert_eq!((measured.rows, measured.net), (1, net));
    assert_eq!(s.all_time.modelled, None);
    assert_eq!(s.week.measured, s.all_time.measured);
}

#[test]
fn without_a_compaction_r_runs_to_the_end_of_the_session_and_a_recent_end_is_a_lower_bound() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    let s = run(&l);
    assert_eq!(s.ledger.recent[0].pair.rounds, Some(4));
    assert_eq!(s.ledger.recent[0].pair.rounds_bound, Some("sessionEnd"));
    assert_eq!(s.all_time.counts.rounds_lower_bound_rows, 0);

    // The same session with a turn ten minutes ago may still be running.
    l.turns.push(turn("t6", "s", NOW - 600_000, SONNET));
    let s = run(&l);
    assert_eq!(s.ledger.recent[0].pair.rounds, Some(5));
    assert_eq!(s.ledger.recent[0].pair.rounds_bound, Some("sessionOpen"));
    assert_eq!(s.all_time.counts.rounds_lower_bound_rows, 1);
}

#[test]
fn the_turn_inside_the_calls_own_second_is_not_the_one_that_read_it() {
    let mut l = derived_ledger();
    // A turn 400 ms into the call's recorded second: the one that made the call, written late.
    l.turns.push(turn("early", "s", at(1, 5) + 400, OPUS));
    let s = run(&l);
    assert_eq!(s.ledger.recent[0].pair.model.as_deref(), Some(SONNET));
}

#[test]
fn a_compaction_before_any_turn_read_the_result_leaves_the_row_with_tokens_and_no_dollars() {
    let mut l = derived_ledger();
    l.boundaries[0].ts_ms = at(1, 7);
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.basis, "clearedBeforeRead");
    assert_eq!((line.dollars, line.net_usd), ("none", None));
    assert_eq!(s.all_time.measured, None);
    assert_eq!(s.all_time.unpriced.no_turn, 1);
    assert_eq!(s.all_time.unpriced.tokens.value, 10_000);
}

/// `n` sessions, each with one call and the turn that read it, priced at a rising context.
fn population(n: usize) -> Ledger {
    let mut l = Ledger {
        events_from: Some(0),
        subagent_events_from: Some(0),
        ..Default::default()
    };
    for i in 0..n {
        let s = format!("s{i}");
        let mut c = call(
            i as i64 + 1,
            Some(&s),
            at(2, 10 * i as i64),
            "smart_read",
            "/r/x.rs",
            5_000,
        );
        c.ts = ts(at(2, 10 * i as i64));
        l.calls.push(c);
        let mut t = turn(&format!("m{i}"), &s, at(2, 10 * i as i64 + 3), SONNET);
        t.cache_read = 10_000 * (i as i64 + 1);
        l.turns.push(t);
        for k in 0..i {
            l.turns
                .push(turn(&format!("m{i}-{k}"), &s, at(3, 10 * k as i64), SONNET));
        }
    }
    l
}

/// No session, no consuming turn, no model: a row whose dollars would be another call's model's
/// price. However many calls could be measured, it carries tokens and no dollars.
#[test]
fn a_row_with_no_session_has_tokens_and_no_dollars_however_many_calls_were_measured() {
    let mut l = population(20);
    let measured_alone = run(&l).all_time.measured.unwrap();
    l.calls
        .push(call(999, None, at(4, 0), "smart_read", "/r/y.rs", 5_000));
    let s = run(&l);
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.pair.basis == "noSession")
        .unwrap();
    assert_eq!(line.tokens_saved.value, 5_000);
    assert_eq!((line.dollars, line.net_usd), ("none", None));
    let u = s.all_time.unpriced;
    assert_eq!((u.calls, u.no_session, u.tokens.value), (1, 1, 5_000));
    // Its tokens count; the dollars are the 20 measured calls' alone.
    assert_eq!(
        s.all_time.tokens_saved.value,
        20 * 5_000 + 5_000,
        "every call's tokens"
    );
    assert_eq!(s.all_time.measured, Some(measured_alone));
    assert!(matches!(
        &s.all_time.data_quality,
        DataQuality::Partial { reason } if reason.contains("1 row with no session recorded")
    ));
}

#[test]
fn a_model_with_no_price_counts_its_tokens_and_no_dollars() {
    let mut l = derived_ledger();
    for t in &mut l.turns {
        t.model = Some("claude-haiku-5-5".into());
    }
    let s = run(&l);
    assert_eq!(s.week.tokens_saved.value, 10_000);
    assert_eq!(s.week.measured, None);
    assert_eq!(s.week.modelled, None);
    assert_eq!((s.week.unpriced.calls, s.week.unpriced.no_price), (1, 1));
    assert_eq!(
        s.week.counts.derived_pairs, 1,
        "the pair is derived, only not priced"
    );
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(line.net_usd, None);
    assert!(
        matches!(s.week.data_quality, DataQuality::Partial { ref reason } if reason.contains("no dollars"))
    );

    // Not borrowed from the other calls either: a call read by a model with no price carries no
    // dollars beside twenty that could be measured.
    let mut l = population(20);
    l.calls.push(call(
        999,
        Some("h"),
        at(5, 0),
        "smart_read",
        "/r/h.rs",
        5_000,
    ));
    l.turns.push(turn("h1", "h", at(5, 3), "claude-haiku-5-5"));
    let s = run(&l);
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.path.as_deref() == Some("/r/h.rs"))
        .unwrap();
    assert_eq!(line.pair.basis, "derived");
    assert_eq!(line.net_usd, None);
}

#[test]
fn a_week_that_lost_is_shown_negative_and_the_status_says_so() {
    let mut l = derived_ledger();
    l.calls[0].saved_tokens = 100;
    l.calls[0].full_tokens = 2_100;
    let s = run(&l);
    let net = s.week.measured.unwrap().net;
    assert!(net.value < 0.0, "{net:?}");
    assert!(net.low <= net.value);
    // Its floor is the most it can have lost; its band's high end, which leaves the modelled
    // round out, is a gain: it may have.
    assert!(net.high > 0.0, "premise: {net:?}");
    assert_eq!(
        s.status,
        Status::NotWorking {
            reason: format!(
                "Lumen may have cost more than it saved: {} this week",
                text::usd_phrase(net.value)
            )
        }
    );
    assert!(text::usd_phrase(net.value).starts_with("net loss of at most $"));
    let line = text::tray_line(&s).unwrap();
    assert!(line.starts_with("Net loss of at most $"), "{line}");
    assert!(line.ends_with("not working"), "{line}");
    // A loss with its rounds cut short by a compaction says so.
    assert_eq!(s.recommendation.unwrap().code, "compactsEarly");
}

#[test]
fn an_empty_ledger_has_no_figures_and_says_nothing_it_did_not_measure() {
    let s = run(&Ledger::default());
    assert_eq!(s.week.data_quality, DataQuality::Empty);
    assert_eq!(s.all_time.data_quality, DataQuality::Empty);
    assert_eq!((s.week.measured, s.week.modelled), (None, None));
    assert_eq!(s.week.share_pct, None);
    assert_eq!(s.all_time.from, None);
    assert_eq!(s.status, Status::NoData);
    assert_eq!(s.recommendation, None);
    assert_eq!(text::tray_line(&s), None);
    assert!(text::headline_line(&s).contains("no Lumen activity recorded yet"));
}

#[test]
fn turns_with_no_lumen_call_are_not_working_and_point_at_doctor() {
    let l = Ledger {
        recent_turns: 40,
        ..Default::default()
    };
    let s = run(&l);
    assert!(matches!(&s.status, Status::NotWorking { reason } if reason.contains("40 rounds")));
    assert_eq!(s.recommendation.unwrap().code, "noCalls");
}

#[test]
fn a_period_with_a_sessionless_row_is_partial_and_says_why() {
    let mut l = derived_ledger();
    l.calls
        .push(call(2, None, at(1, 30), "smart_read", "/r/b.rs", 1_000));
    let s = run(&l);
    match &s.week.data_quality {
        DataQuality::Partial { reason } => {
            assert!(reason.contains("1 row with no session"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.week.counts.calls, 2);
    assert_eq!(s.week.tokens_saved.value, 11_000);

    let full = run(&derived_ledger());
    assert!(
        matches!(full.week.data_quality, DataQuality::Complete),
        "one derived, priced, measured row: {:?}",
        full.week.data_quality
    );
}

#[test]
fn an_unverified_count_and_a_loader_gap_make_the_period_partial() {
    let mut l = derived_ledger();
    l.calls[0].token_source = None;
    l.gaps.push("the fault spool could not be read".into());
    let s = run(&l);
    let DataQuality::Partial { reason } = &s.all_time.data_quality else {
        panic!("{:?}", s.all_time.data_quality)
    };
    assert!(reason.contains("not known to be measured") && reason.contains("fault spool"));
    assert_eq!(s.all_time.counts.unverified_provenance_rows, 1);
}

#[test]
fn a_recall_after_an_outline_of_the_same_file_costs_a_round_on_evidence() {
    let mut l = derived_ledger();
    l.calls.push(call(
        2,
        Some("s"),
        at(1, 65),
        "recall_file",
        "/r/a.rs",
        9_000,
    ));
    l.calls.push(call(
        3,
        Some("s"),
        at(1, 70),
        "recall_file",
        "/r/other.rs",
        9_000,
    ));
    let s = run(&l);
    let by_route = |path: &str| {
        s.ledger
            .recent
            .iter()
            .find(|x| x.path.as_deref() == Some(path) && x.tool == Some("recall_file"))
            .unwrap()
    };
    assert_eq!(by_route("/r/a.rs").extra_round_basis, "followUp");
    assert!(by_route("/r/a.rs").extra_round_evidenced);
    assert_eq!(
        by_route("/r/other.rs").extra_round_basis,
        "blockNotRecorded"
    );
}

#[test]
fn a_repeat_answer_costs_no_round_and_a_fresh_ask_after_a_stub_costs_one() {
    let mut l = derived_ledger();
    l.calls.push(call(
        2,
        Some("s"),
        at(1, 65),
        ROUTE_UNCHANGED,
        "/r/a.rs",
        9_900,
    ));
    let mut fresh = call(3, Some("s"), at(1, 70), "smart_read", "/r/a.rs", 9_000);
    fresh.repeat_plan = Some(PLAN_FRESH.into());
    fresh.repeat_arm = Some(ARM_TREATMENT.into());
    l.calls.push(fresh);
    let s = run(&l);
    let stub = s
        .ledger
        .recent
        .iter()
        .find(|x| x.route.as_deref() == Some(ROUTE_UNCHANGED))
        .unwrap();
    assert_eq!(stub.extra_rounds, 0);
    assert_eq!(stub.extra_round_basis, "repeatAnswer");
    assert!(close(stub.extra_round_usd.unwrap().value, 0.0));
    let again = s
        .ledger
        .recent
        .iter()
        .find(|x| x.extra_round_basis == "freshAfterStub")
        .unwrap();
    assert!(again.extra_round_evidenced);
}

/// A stub stands in for the answer Lumen would have sent again, the outline, not for the file:
/// the ledger records 9,900 saved against a 11,900-token file, and the outline it replaced was
/// 2,000, so 1,970 is what it saved.
#[test]
fn a_repeat_read_is_counted_against_the_answer_it_replaced_not_the_whole_file() {
    let mut l = derived_ledger();
    // The first read's outline: 12,000 in the file, 2,000 returned.
    let mut stub = call(2, Some("s"), at(1, 65), ROUTE_UNCHANGED, "/r/a.rs", 11_970);
    stub.full_tokens = 12_000;
    stub.returned_tokens = 30;
    l.calls.push(stub);
    // A recall stub with an earlier answer to the same request (its req_key), and one whose
    // earlier answer is not in the ledger.
    let mut recall = call(3, Some("s"), at(1, 66), "recall_file", "/r/b.rs", 7_000);
    recall.req_key = Some("/r/b.rs#names=f&range=-".into());
    recall.full_tokens = 9_000;
    recall.returned_tokens = 2_000;
    l.calls.push(recall.clone());
    let mut recall_stub = call(
        4,
        Some("s"),
        at(1, 67),
        ROUTE_RECALL_UNCHANGED,
        "/r/b.rs",
        8_960,
    );
    recall_stub.req_key = recall.req_key.clone();
    recall_stub.full_tokens = 9_000;
    recall_stub.returned_tokens = 40;
    l.calls.push(recall_stub);
    let mut orphan = call(5, Some("s"), at(1, 68), ROUTE_UNCHANGED, "/r/c.rs", 5_970);
    orphan.full_tokens = 6_000;
    orphan.returned_tokens = 30;
    l.calls.push(orphan);
    let s = run(&l);
    let line = |rowid_saved: i64| {
        s.ledger
            .recent
            .iter()
            .find(|x| x.recorded_saved_cl100k == rowid_saved)
            .unwrap()
    };
    let a = line(11_970);
    assert_eq!(a.saved_against, "previousAnswer");
    assert_eq!(a.saved_cl100k, 2_000 - 30);
    assert_eq!(a.tokens_saved.value, 1_970);
    let b = line(8_960);
    assert_eq!(b.saved_against, "previousAnswer");
    assert_eq!(b.saved_cl100k, 2_000 - 40);
    let c = line(5_970);
    assert_eq!(c.saved_against, "answerNotInLedger");
    assert_eq!(
        (
            c.tokens_saved.low,
            c.tokens_saved.value,
            c.tokens_saved.high
        ),
        (0, 0, 5_970),
        "none counted, the whole file at most"
    );
    // The week counts 10,000 + 1,970 + 7,000 + 1,960 + 0, not the ledger's 43,900.
    assert_eq!(s.week.tokens_saved.value, 10_000 + 1_970 + 7_000 + 1_960);
}

#[test]
fn compress_logs_on_inline_text_saves_nothing_and_pays_for_writing_the_text_out() {
    let mut l = derived_ledger();
    let mut inline = call(2, Some("s"), at(1, 65), "compress_logs", INLINE_PATH, 8_000);
    inline.full_tokens = 10_000;
    inline.returned_tokens = 2_000;
    l.calls.push(inline);
    let s = run(&l);
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.extra_round_basis == "inlineText")
        .unwrap();
    assert_eq!(line.saved_cl100k, -2_000, "only its reply was added");
    let net = line.net_usd.unwrap();
    assert!(net.high < 0.0, "a loss at every end: {net:?}");
    // The round, and the 10,000 tokens of text written out as output at Sonnet 4.6's $15.
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    let cost = line.extra_round_usd.unwrap();
    assert!(close(cost.value, c + 10_000.0 * 15.0 / 1e6), "{cost:?}");
    // And none of the text counts as something a read would have put in context.
    assert_eq!(s.week.tokens_full.value, 12_000);
}

#[test]
fn a_subagents_call_is_read_by_its_own_turns_and_not_the_main_agents() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    let sub = |id: &str, ms: i64| TurnRow {
        is_subagent: true,
        source_file: Some("/h/.claude/projects/-p/s/subagents/agent-abc.jsonl".into()),
        ..turn(id, "s", ms, OPUS)
    };
    l.turns.push(sub("a1", at(1, 20)));
    l.turns.push(sub("a2", at(1, 30)));
    let mut c = call(2, Some("s"), at(1, 15), "smart_read", "/r/sub.rs", 4_000);
    c.agent_id = Some("abc".into());
    l.calls.push(c);
    let mut lost = call(3, Some("s"), at(1, 15), "smart_read", "/r/sub2.rs", 4_000);
    lost.agent_id = Some("not-in-the-ledger".into());
    l.calls.push(lost);
    let s = run(&l);
    let find = |p: &str| {
        s.ledger
            .recent
            .iter()
            .find(|x| x.path.as_deref() == Some(p))
            .unwrap()
    };
    let line = find("/r/sub.rs");
    assert_eq!(line.pair.model.as_deref(), Some(OPUS));
    assert_eq!(
        line.pair.rounds,
        Some(1),
        "a2 only; the main agent's turns are not its rounds"
    );
    assert_eq!(find("/r/sub2.rs").pair.basis, "subagentNotInLedger");
    // And the main agent's call is not read by the subagent's turn at 12:00:20.
    assert_eq!(find("/r/a.rs").pair.rounds, Some(4));
}

/// The layout on a real machine: `<project>/<session>/subagents/agent-<id>.jsonl`, with the bare
/// id in `read_events.agent_id`, under either separator, and a compaction in the subagent's own
/// transcript ending only its R.
#[test]
fn a_subagent_transcript_is_matched_by_its_file_name_under_either_separator() {
    let session = "0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";
    let unix = format!(
        "/Users/me/.claude/projects/-Users-me-dev-app/{session}/subagents/agent-a0223eddc41f13056.jsonl"
    );
    let windows = format!(
        r"C:\Users\me\.claude\projects\C--dev-app\{session}\subagents\agent-a31afcc8757770e62.jsonl"
    );
    let sub = |id: &str, ms: i64, file: &str, flagged: bool| TurnRow {
        is_subagent: flagged,
        source_file: Some(file.to_string()),
        ..turn(id, session, ms, OPUS)
    };
    let mut l = Ledger {
        turns: vec![
            turn("m0", session, at(1, 0), SONNET),
            turn("m1", session, at(1, 30), SONNET),
            turn("m2", session, at(1, 90), SONNET),
            sub("u1", at(1, 20), &unix, true),
            sub("u2", at(1, 40), &unix, true),
            sub("u3", at(1, 60), &unix, true),
            sub("u4", at(1, 80), &unix, true),
            // An old Windows row the forward-slash backfill never marked.
            sub("w1", at(1, 25), &windows, false),
            sub("w2", at(1, 45), &windows, false),
        ],
        boundaries: vec![BoundaryRow {
            session_id: session.into(),
            kind: "subagent_compaction".into(),
            ts_ms: at(1, 70),
            source_file: Some(unix.clone()),
        }],
        events_from: Some(0),
        subagent_events_from: Some(0),
        ..Default::default()
    };
    let mut on_unix = call(1, Some(session), at(1, 15), "smart_read", "/r/u.rs", 4_000);
    on_unix.agent_id = Some("a0223eddc41f13056".into());
    let mut on_windows = call(2, Some(session), at(1, 15), "smart_read", "/r/w.rs", 4_000);
    on_windows.agent_id = Some("a31afcc8757770e62".into());
    let main = call(3, Some(session), at(1, 15), "smart_read", "/r/m.rs", 4_000);
    l.calls = vec![on_unix, on_windows, main];
    let s = run(&l);
    let find = |p: &str| {
        s.ledger
            .recent
            .iter()
            .find(|x| x.path.as_deref() == Some(p))
            .unwrap()
    };
    // Read by u1, then u2 and u3 before its own compaction at 70 s; u4 is after it.
    let u = find("/r/u.rs");
    assert_eq!(u.pair.model.as_deref(), Some(OPUS));
    assert_eq!(
        (u.pair.rounds, u.pair.rounds_bound),
        (Some(2), Some("compaction"))
    );
    // Read by w1, then w2: the subagent's compaction is not its own.
    let w = find("/r/w.rs");
    assert_eq!(w.pair.model.as_deref(), Some(OPUS));
    assert_eq!(
        (w.pair.rounds, w.pair.rounds_bound),
        (Some(1), Some("sessionEnd"))
    );
    // The main agent's own turns: read by m1, then m2; no subagent turn and no subagent
    // compaction counts for it.
    let m = find("/r/m.rs");
    assert_eq!(m.pair.model.as_deref(), Some(SONNET));
    assert_eq!(
        (m.pair.rounds, m.pair.rounds_bound),
        (Some(1), Some("sessionEnd"))
    );
}

#[test]
fn a_blocked_read_the_valve_let_through_costs_the_round_that_retried_it() {
    let mut l = derived_ledger();
    l.blocked_reads.push(BlockedReadRow {
        ts: ts(at(1, 61)),
        session_id: Some("s".into()),
        path: Some("/r/big.rs".into()),
        detail: None,
        agent_id: None,
        version: None,
    });
    let s = run(&l);
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.kind == LineKind::BlockedRead)
        .unwrap();
    assert_eq!(line.pair.model.as_deref(), Some(SONNET));
    assert_eq!(line.pair.rounds, None);
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    let net = line.net_usd.unwrap();
    assert!(
        close(net.value, -c) && close(net.high, -c) && close(net.low, -c),
        "{net:?}"
    );
    assert_eq!(s.all_time.counts.blocked_reads, 1);
    assert_eq!(s.all_time.counts.extra_rounds_evidenced, 1);
}

#[test]
fn an_unrecorded_speed_region_and_cache_duration_make_the_dollars_a_floor() {
    let mut l = derived_ledger();
    for t in &mut l.turns {
        t.model = Some(OPUS.into());
        t.speed = None;
        t.inference_geo = None;
        t.cache_5m = None;
        t.cache_1h = None;
    }
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.ttl, Some("unrecorded"));
    assert_eq!(line.pricing_unknown, ["speed", "region", "cacheDuration"]);
    let v = line.value_usd.unwrap();
    // Value: at the floor, as the app prices any such turn (standard, global, 5-minute writes);
    // high at fast mode's prices, US-only, 1-hour writes.
    assert!(close(v.value, 10_000.0 * (6.25 + 2.0 * 0.5) / 1e6), "{v:?}");
    assert!(
        close(v.high, 10_000.0 * (20.0 + 2.0 * 1.0) * 1.1 / 1e6),
        "{v:?}"
    );
    let c = line.extra_round_usd.unwrap();
    // The round: central at the ceiling, so the net is a floor; low at the floor.
    let c_lo = (10.0 * 5.0 + 500.0 * 25.0 + 50_000.0 * 0.5 + 3_000.0 * 6.25) / 1e6;
    let c_hi = (10.0 * 10.0 + 500.0 * 50.0 + 50_000.0 * 1.0 + 3_000.0 * 20.0) * 1.1 / 1e6;
    assert!(
        close(c.value, c_hi) && close(c.high, c_hi) && close(c.low, 0.0),
        "{c:?}"
    );
    assert!(c_lo < c_hi);

    // The reasons survive into the period, in the app's words, and the figure is a floor: for a
    // loss, the most it can be.
    for want in [
        rates::Shortfall::SpeedUnknown,
        rates::Shortfall::GeoUnknown,
        rates::Shortfall::DurationUnknown,
    ] {
        assert!(s.week.usd_shortfalls.contains(&want.describe()), "{want:?}");
    }
    let tray = text::tray_line(&s).unwrap();
    assert!(tray.starts_with("Net loss of at most $"), "{tray}");
    assert!(text::headline_line(&s).contains("net loss of at most $"));
    // A floor that is a loss while its band reaches a gain is a loss that may not have been.
    let net = s.week.measured.unwrap().net;
    assert!(net.value < 0.0 && net.high > 0.0, "premise: {net:?}");
    assert_eq!(
        s.status,
        Status::NotWorking {
            reason: format!(
                "Lumen may have cost more than it saved: {} this week",
                text::usd_phrase(net.value)
            )
        }
    );
}

#[test]
fn a_recorded_us_only_turn_costs_1_1_times_and_is_exact() {
    let mut l = derived_ledger();
    for t in &mut l.turns {
        t.inference_geo = Some(rates::GEO_US.into());
    }
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.region.as_deref(), Some("us"));
    assert!(line.pricing_unknown.is_empty());
    let v = line.value_usd.unwrap();
    assert!(
        close(v.value, 10_000.0 * (3.75 + 2.0 * 0.3) * 1.1 / 1e6),
        "{v:?}"
    );
    assert!(
        close(v.value, v.high),
        "nothing unrecorded: no band from prices"
    );
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) * 1.1 / 1e6;
    assert!(close(line.extra_round_usd.unwrap().value, c));
    // Every price recorded, and still a floor: the first smart_read's round is counted though a
    // blocked Read before it is not on record.
    assert!(s.week.usd_shortfalls.is_empty());
    assert!(text::tray_line(&s).unwrap().starts_with("Saved at least $"));

    // "not_available", what Claude Code writes with no region, is priced as global, a floor.
    for t in &mut l.turns {
        t.inference_geo = Some(rates::GEO_NOT_AVAILABLE.into());
    }
    let s = run(&l);
    assert_eq!(s.ledger.recent[0].pricing_unknown, ["region"]);
    assert!(!s.week.usd_shortfalls.is_empty());
}

#[test]
fn the_identity_hook_recommendation_follows_the_ledger_over_the_settings() {
    let mut l = derived_ledger();
    l.calls[0].repeat_plan = Some(PLAN_UNIDENTIFIED.into());
    let s = run(&l);
    assert_eq!(s.recommendation.unwrap().code, "identityHook");

    // An identified plan proves the hook reaches the server whatever settings.json says.
    l.calls[0].repeat_plan = Some("first".into());
    let s = summarize(
        &l,
        &clock(),
        &Facts {
            billing_mode: BillingView::Api,
            identity_hook_registered: Some(false),
        },
        &Uncalibrated,
    );
    assert_eq!(s.recommendation, None);
}

#[test]
fn a_call_before_the_week_is_in_all_time_only() {
    let mut l = derived_ledger();
    l.calls.push(call(
        2,
        None,
        MONDAY - 3_600_000,
        "smart_read",
        "/r/old.rs",
        700,
    ));
    let s = run(&l);
    assert_eq!(s.week.counts.calls, 1);
    assert_eq!(s.all_time.counts.calls, 2);
    assert_eq!(
        s.all_time.from.as_deref(),
        Some(iso_ms(MONDAY - 3_600_000).as_str())
    );
    assert_eq!(s.week.from.as_deref(), Some("2026-10-12T00:00:00Z"));
}

#[test]
fn a_subscription_line_has_its_tokens_and_the_dollars_at_api_prices() {
    let l = derived_ledger();
    let api = run(&l);
    assert!(
        text::tray_line(&api)
            .unwrap()
            .starts_with("Saved at least $0.0")
    );
    let sub = summarize(
        &l,
        &clock(),
        &Facts {
            billing_mode: BillingView::Subscription,
            identity_hook_registered: Some(true),
        },
        &Uncalibrated,
    );
    let usd = text::usd_signed(sub.week.measured.unwrap().net.value);
    let share = text::share_phrase(&sub.week).unwrap();
    assert_eq!(
        text::tray_line(&sub).unwrap(),
        format!(
            "This week: {share}; \u{2248}\u{a0}10,000 tokens saved, net; at least {usd} saved, net at \
             API prices \u{b7} working"
        )
    );
    let line = text::headline_line(&sub);
    assert!(
        line.contains(&format!("at least {usd} saved, net at API prices")),
        "{line}"
    );
    assert!(
        line.contains("fewer tokens from file and tool reads"),
        "{line}"
    );
    assert!(text::headline_line(&api).contains("at least $"));
    assert!(!text::headline_line(&api).contains("at API prices"));
    assert!(!text::headline_line(&api).contains("modelled"));
}

/// A calibrator that adds half, with a band, for the seam.
struct Half;
impl Calibrator for Half {
    fn claude_tokens(&self, n: u64, _: &str, _: Option<&str>) -> Band<u64> {
        Band {
            low: n * 14 / 10,
            value: n * 15 / 10,
            high: n * 16 / 10,
        }
    }
    fn info(&self) -> CalibrationInfo {
        CalibrationInfo {
            calibrated: true,
            as_of: Some("test".into()),
            note: String::new(),
        }
    }
}

#[test]
fn a_calibrator_scales_tokens_and_dollars_and_keeps_a_loss_negative() {
    let mut l = derived_ledger();
    l.calls.push(call(
        2,
        Some("s"),
        at(1, 65),
        "smart_read_full",
        "/r/c.rs",
        -40,
    ));
    let s = summarize(&l, &clock(), &facts(), &Half);
    let gain = s
        .ledger
        .recent
        .iter()
        .find(|x| x.saved_cl100k == 10_000)
        .unwrap();
    assert_eq!(
        gain.tokens_saved,
        Band {
            low: 14_000,
            value: 15_000,
            high: 16_000
        }
    );
    let loss = s
        .ledger
        .recent
        .iter()
        .find(|x| x.saved_cl100k == -40)
        .unwrap();
    assert_eq!(
        loss.tokens_saved,
        Band {
            low: -64,
            value: -60,
            high: -56
        }
    );
    assert!(s.method.calibration.calibrated);
    let un = run(&l);
    assert!(!un.method.calibration.calibrated);
}

#[test]
fn timestamps_parse_in_every_shape_the_ledger_writes_them() {
    let base = parse_ts_ms("2026-06-01T16:14:05Z").unwrap();
    assert_eq!(parse_ts_ms("2026-06-01T16:14:05.157Z"), Some(base + 157));
    assert_eq!(parse_ts_ms("2026-06-01 16:14:05"), Some(base));
    assert_eq!(parse_ts_ms("2026-06-01T18:14:05+02:00"), Some(base));
    assert_eq!(parse_ts_ms("2026-06-01T16:14:05.5Z"), Some(base + 500));
    assert_eq!(iso_ms(base), "2026-06-01T16:14:05Z");
    assert_eq!(iso_ms(MONDAY), "2026-10-12T00:00:00Z");
    for bad in [
        "",
        "yesterday",
        "2026-13-01T00:00:00Z",
        "2026-06-01T16:14",
        "2026-06-01T16:14:05Q",
    ] {
        assert_eq!(parse_ts_ms(bad), None, "{bad}");
    }
}

#[test]
fn every_route_the_server_writes_is_a_call_route_and_the_sql_reads_the_same_list() {
    use crate::ranked::Decline;
    for d in [
        Decline::NoQuery,
        Decline::NoDefs,
        Decline::NotWorthIt,
        Decline::WouldInflate,
        Decline::TooSlow,
    ] {
        assert!(CALL_ROUTES.contains(&d.route()), "{}", d.route());
    }
    assert!(CALL_ROUTES.contains(&crate::ranked::ROUTE_RANKED));
    for r in REPEAT_ROUTES {
        assert!(CALL_ROUTES.contains(&r));
    }
    for r in CALL_ROUTES {
        assert!(
            load::CALLS_SQL.contains(&format!("'{r}'")),
            "{r} missing from CALLS_SQL"
        );
    }
    assert_eq!(load::CALLS_SQL.matches('\'').count(), CALL_ROUTES.len() * 2);
    assert!(!CALL_ROUTES.contains(&"builtin_read") && !CALL_ROUTES.contains(&"bash_output"));
}

/// A ledger from before 1.9.0 has a `faults` table without `agent_id`, which the summary reads and
/// the drain writes: the rusqlite opener adds it, and the summary reads the old rows.
#[test]
fn the_rusqlite_opener_gives_a_1_8_0_faults_table_its_agent_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    let conn = crate::meter::connect_db(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE faults;
         CREATE TABLE faults (ts TEXT NOT NULL, kind TEXT NOT NULL,
             variant TEXT NOT NULL DEFAULT '-', path TEXT, lines INTEGER, detail TEXT,
             session_id TEXT, version TEXT, channel TEXT NOT NULL DEFAULT 'unknown');
         INSERT INTO faults (ts, kind, variant, path, session_id, version) VALUES
             ('2026-10-12T01:00:20Z', 'hook_fail_open', 'retry_escape_valve', '/r/b.rs', 's',
              '1.8.0');",
    )
    .unwrap();
    drop(conn);
    let conn = crate::meter::connect_db(&db).unwrap();
    let has: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('faults') WHERE name = 'agent_id'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n == 1)
        .unwrap();
    assert!(has);
    let (ledger, _) = load::load(&conn, NOW, None).unwrap();
    assert_eq!(ledger.blocked_reads.len(), 1);
    assert_eq!(ledger.blocked_reads[0].version.as_deref(), Some("1.8.0"));
    // And the drain can write the column the hook now fills.
    let spool = dir.path().join("faults.jsonl");
    let mut rec = crate::faults::FaultRecord::now("hook_fail_open", "retry_escape_valve");
    rec.agent_id = Some("a1".into());
    crate::faults::record_at(&spool, &rec);
    assert_eq!(crate::faults::drain_spool_at(&conn, &spool).unwrap(), 1);
}

#[test]
fn the_rusqlite_loader_reads_a_real_schema_and_the_week_starts_on_a_local_monday() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    let conn = crate::meter::connect_db(&db).unwrap();
    conn.execute_batch(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,
                                 routed_via,channel,session_id,token_source)
         VALUES('2026-10-12T01:00:05Z','mcp__lumen__smart_read','/r/a.rs',400,2000,12000,10000,
                'smart_read','cli','s','measured'),
               ('2026-10-12T01:00:06Z','Read','/r/a.rs',400,12000,12000,0,
                'builtin_read','cli','s','measured');
         INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,
                                 routed_via,channel,session_id,token_source,agent_id)
         VALUES('2026-10-12T01:00:21.400Z','Read','/r/b.rs',400,12000,12000,0,
                'builtin_read','cli','s','measured','a7'),
               ('2026-10-12T01:00:21Z','Read','/r/c.rs',400,100,100,0,
                'builtin_read','cli','elsewhere','measured',NULL);
         INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                           cache_read_input_tokens,cache_creation_input_tokens,cache_5m,cache_1h,speed)
         VALUES('t1','s','2026-10-12T01:00:09.250Z','claude-sonnet-4-6',10,500,50000,3000,3000,0,'standard'),
               ('other','x','2026-10-12T01:00:09Z','claude-sonnet-4-6',1,1,1,1,NULL,NULL,NULL);
         INSERT INTO session_events(session_id,kind,uuid,ts_ms,source_file)
         VALUES('s','compaction','u1',1791766815000,NULL),
               ('s','rewind','u2',1791766816000,'/h/.claude/projects/-p/s.jsonl'),
               ('s','subagent_compaction','u3',1791766817000,
                '/h/.claude/projects/-p/s/subagents/agent-x.jsonl'),
               ('s','resync','resync-1',1791766700000,'/h/.claude/projects/-p/s.jsonl'),
               ('x','compaction','u4',1791766600000,
                '/h/.claude/projects/-p/x/subagents/agent-y.jsonl');
         INSERT INTO faults(ts,kind,variant,path,session_id,detail)
         VALUES('2026-10-12T01:00:20Z','hook_fail_open','retry_escape_valve','/r/b.rs','s',
                'after_lumen_read');
         INSERT INTO faults(ts,kind,variant,path,session_id,agent_id,version)
         VALUES('2026-10-12T01:00:25Z','hook_fail_open','retry_escape_valve','/r/b.rs','s',
                'a8','1.9.0');",
    )
    .unwrap();
    let spool = dir.path().join("faults.jsonl");
    std::fs::write(
        &spool,
        "{\"ts\":\"2026-10-12T01:00:30Z\",\"kind\":\"hook_fail_open\",\"variant\":\"retry_escape_valve\",\"session_id\":\"s\",\"agent_id\":\"a9\"}\n\
         {\"ts\":\"2026-10-12T01:00:31Z\",\"kind\":\"panic\",\"variant\":\"-\"}\n",
    )
    .unwrap();
    let (ledger, clock) = load::load(&conn, NOW, Some(&spool)).unwrap();
    assert_eq!(
        ledger.calls.len(),
        1,
        "the built-in Read is not a Lumen call"
    );
    assert_eq!(ledger.sessions.len(), 1, "only the sessions a call names");
    let t = &ledger.sessions[0].turns[0];
    assert_eq!((t.cache_5m, t.cache_1h), (Some(3000), Some(0)));
    // SQLite reads the transcript's time, milliseconds included.
    assert_eq!(t.ts_ms, Some(MONDAY + 3_600_000 + 9_250));
    let class = &ledger.classes[t.class as usize];
    assert_eq!(class.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(class.speed.as_deref(), Some("standard"));
    // Every kind that ends R, and no resync; only the sessions a call names.
    let kinds: Vec<&str> = ledger.boundaries.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(kinds, ["compaction", "rewind", "subagent_compaction"]);
    // Recording began with the resync, the earliest main-transcript event; a compaction filed
    // under a subagent's transcript is not one.
    assert_eq!(ledger.events_from, Some(1_791_766_700_000));
    assert_eq!(ledger.subagent_events_from, Some(1_791_766_817_000));
    assert_eq!(ledger.blocked_reads.len(), 3, "the table's and the spool's");
    assert_eq!(
        ledger.blocked_reads[0].detail.as_deref(),
        Some(AFTER_LUMEN_READ)
    );
    assert_eq!(ledger.blocked_reads[1].agent_id.as_deref(), Some("a8"));
    assert_eq!(ledger.blocked_reads[1].version.as_deref(), Some("1.9.0"));
    assert_eq!(ledger.blocked_reads[2].agent_id.as_deref(), Some("a9"));
    // The meter's Reads of the sessions whose faults name no agent, and when it began naming
    // agents.
    // Only of the paths those faults name: /r/a.rs was read in full and never blocked, and
    // /r/c.rs was read in another session.
    assert_eq!(
        ledger.valve_reads,
        [ValveReadRow {
            session_id: "s".into(),
            path: "/r/b.rs".into(),
            ts_ms: Some(MONDAY + 3_600_000 + 21_400),
            agent_id: Some("a7".into()),
        }]
    );
    assert_eq!(ledger.meter_agents_from, Some(MONDAY + 3_600_000 + 21_400));
    assert!(ledger.gaps.is_empty());
    // Monday 00:00 local: within a day of the UTC Monday, and a Monday.
    assert!(
        (clock.week_start_ms - MONDAY).abs() <= 14 * 3_600_000,
        "{clock:?}"
    );
    assert_eq!(clock.week_start, "2026-10-12");
    let s = summarize(&ledger, &clock, &facts(), &Uncalibrated);
    assert_eq!(s.all_time.counts.calls, 1);
    // Three blocks: a7's (named by the meter's row for its Read), a8's and a9's.
    assert_eq!(s.all_time.counts.blocked_reads, 3);
    assert_eq!(s.all_time.counts.blocked_reads_bounded, 0);
    assert_eq!(
        s.ledger
            .recent
            .iter()
            .find(|x| x.kind == LineKind::Call)
            .unwrap()
            .pair
            .basis,
        "derived"
    );
}

#[test]
fn the_summary_serializes_with_camel_case_names_the_frontend_reads() {
    let s = run(&derived_ledger());
    let v = serde_json::to_value(&s).unwrap();
    for key in [
        "schema",
        "asOf",
        "billingMode",
        "week",
        "allTime",
        "status",
        "recommendation",
        "method",
        "ledger",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    let week = &v["week"];
    for key in [
        "from",
        "to",
        "tokensSaved",
        "tokensFull",
        "sharePct",
        "measured",
        "modelled",
        "unpriced",
        "usdShortfalls",
        "counts",
        "dataQuality",
    ] {
        assert!(week.get(key).is_some(), "week.{key}");
    }
    assert_eq!(v["status"]["state"], "working");
    assert_eq!(week["dataQuality"]["state"], "complete");
    assert_eq!(v["billingMode"], "api");
    for key in ["low", "value", "high"] {
        assert!(week["measured"]["net"].get(key).is_some());
    }
    assert!(
        v["ledger"]["recent"][0]["pair"]
            .get("roundsBound")
            .is_some()
    );
}

// ── R's end ──────────────────────────────────────────────────────────────────

/// A compaction the daemon never recorded: a later prompt is smaller than the one before it, so
/// context was taken out, and the result may have gone with it. R ends there, labelled as
/// inferred.
#[test]
fn a_prompt_smaller_than_the_one_before_it_ends_r_as_a_collapse() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    // t3 sends 10 + 3,000 + 1,000 tokens, less than t1's 53,010.
    l.turns[3].cache_read = 1_000;
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(1), Some("collapse")),
        "t2 only"
    );
    assert!(
        line.pair.rounds_trusted,
        "a recorded conversation's collapse"
    );
    assert_eq!(s.all_time.counts.rounds_collapse_rows, 1);
    let value = line.value_usd.unwrap();
    assert!(
        close(value.value, 10_000.0 * (3.75 + 0.3) / 1e6),
        "{value:?}"
    );
    // A long session's summary can be larger than the prompt the result entered: the shrink
    // from 90,010 to 60,010 still ends R.
    l.turns[2].cache_read = 87_000;
    l.turns[3].cache_read = 57_000;
    l.turns[4].cache_read = 58_000;
    l.turns[5].cache_read = 59_000;
    let line = &run(&l).ledger.recent[0];
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(1), Some("collapse"))
    );
    // A prompt that only grows is no collapse.
    l.turns[3].cache_read = 87_000;
    l.turns[4].cache_read = 87_000;
    l.turns[5].cache_read = 87_500;
    assert_eq!(
        run(&l).ledger.recent[0].pair.rounds_bound,
        Some("sessionEnd")
    );
    // A recorded compaction before the collapse ends R first.
    let mut l = derived_ledger();
    l.turns[5].cache_read = 1_000;
    assert_eq!(
        run(&l).ledger.recent[0].pair.rounds_bound,
        Some("compaction")
    );
}

/// A conversation that began before the daemon recorded events may have compacted unrecorded:
/// its R may be too long. Its saving is measured at R = 0, the least it can be worth, with its
/// rounds in full; what it is worth beyond that is modelled, beside the measured figure.
#[test]
fn a_conversation_older_than_the_event_record_is_measured_at_r_0_and_the_rest_modelled() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    l.events_from = Some(at(2, 0));
    let s = run(&l);
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.path.as_deref() == Some("/r/a.rs"))
        .unwrap();
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(4), Some("sessionEndUnrecorded"))
    );
    assert!(!line.pair.rounds_trusted);
    assert_eq!(line.dollars, "measured");
    assert_eq!(s.all_time.counts.rounds_unrecorded_rows, 1);
    // At R = 0: the 10,000 tokens written once at $3.75. Beyond it: four cache reads at $0.30.
    assert!(close(line.value_usd.unwrap().value, 10_000.0 * 3.75 / 1e6));
    let rest = line.modelled_value_usd.unwrap();
    assert!(close(rest.value, 10_000.0 * 4.0 * 0.3 / 1e6), "{rest:?}");
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    assert!(
        close(line.extra_round_usd.unwrap().value, c),
        "the round in full"
    );
    let measured = s.all_time.measured.unwrap();
    assert_eq!(measured.net, line.net_usd.unwrap());
    assert!(measured.high_open, "the rest is not in its high end");
    let modelled = s.all_time.modelled.unwrap();
    assert_eq!((modelled.rows, modelled.value), (1, rest));
    let tray = text::tray_line(&s).unwrap();
    assert!(
        tray.starts_with(&format!(
            "Saved at least {} this week (+$0.01 modelled) \u{b7} ",
            text::usd_signed(measured.net.value)
        )),
        "{tray}"
    );

    // A loss grows with R too, so it is measured at R, in full, and nothing of it is modelled:
    // inline text for compress_logs in the same conversation.
    let mut inline = call(2, Some("s"), at(1, 65), "compress_logs", INLINE_PATH, 8_000);
    inline.full_tokens = 10_000;
    inline.returned_tokens = 2_000;
    l.calls.push(inline);
    let s = run(&l);
    let loss = s
        .ledger
        .recent
        .iter()
        .find(|x| x.extra_round_basis == "inlineText")
        .unwrap();
    assert_eq!(loss.pair.rounds_bound, Some("sessionEndUnrecorded"));
    assert_eq!(loss.dollars, "measured");
    // −2,000 tokens written once and read on the two rounds after the turn that read it (t3).
    let v = loss.value_usd.unwrap().value;
    assert!(close(v, -2_000.0 * (3.75 + 2.0 * 0.3) / 1e6), "{v}");
    assert_eq!(loss.modelled_value_usd.map(|b| b.value), Some(0.0));
    let net = loss.net_usd.unwrap().value;
    assert!(net < -0.05, "a certain loss: {net}");
    assert_eq!(
        s.all_time.modelled.unwrap().rows,
        1,
        "only the saving's rest"
    );
    let both = s.all_time.measured.unwrap();
    assert_eq!(both.rows, 2);
    assert!(close(both.net.value, measured.net.value + net));

    // Nothing recorded at all is the same.
    l.calls.truncate(1);
    l.events_from = None;
    assert_eq!(
        run(&l).ledger.recent[0].pair.rounds_bound,
        Some("sessionEndUnrecorded")
    );
    // A collapse there vouches for the end: every compaction shrinks the prompt, so none came
    // before it unseen.
    l.turns[3].cache_read = 1_000;
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(line.pair.rounds_bound, Some("collapse"));
    assert!(line.pair.rounds_trusted);
    assert_eq!(s.all_time.counts.rounds_unrecorded_rows, 0);
    assert_eq!(line.modelled_value_usd, None);
    assert!(s.all_time.measured.is_some() && s.all_time.modelled.is_none());
}

#[test]
fn a_recorded_rewind_ends_r_and_a_resync_ends_nothing() {
    let mut l = derived_ledger();
    l.boundaries[0].kind = "rewind".into();
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(2), Some("rewind"))
    );
    assert!(line.pair.rounds_trusted);
    l.boundaries[0].kind = "resync".into();
    assert_eq!(
        (
            run(&l).ledger.recent[0].pair.rounds,
            run(&l).ledger.recent[0].pair.rounds_bound
        ),
        (Some(4), Some("sessionEnd"))
    );
}

/// `<synthetic>` is a message Claude Code wrote itself: no request, so no round, and its empty
/// usage is no collapse.
#[test]
fn a_synthetic_message_is_neither_a_round_nor_a_collapse() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    let mut synthetic = turn("syn", "s", at(1, 90), "<synthetic>");
    synthetic.input = 0;
    synthetic.output = 0;
    synthetic.cache_read = 0;
    synthetic.cache_write = 0;
    l.turns.push(synthetic);
    let line = &run(&l).ledger.recent[0];
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(4), Some("sessionEnd"))
    );
}

/// The same, on the loaders' path: turns packed per session, their model named once.
#[test]
fn a_synthetic_message_the_loaders_read_is_neither_a_round_nor_a_collapse() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    let loaded = |id: i64, secs: i64, class: u32, cache_read: i64| LoadedTurn {
        id,
        ts_ms: Some(at(1, secs)),
        class,
        input: 10,
        output: 500,
        cache_read,
        cache_write: 3_000,
        cache_5m: Some(3_000),
        cache_1h: Some(0),
        is_subagent: false,
        source_file: None,
    };
    l.classes = vec![
        TurnClass {
            model: Some(SONNET.into()),
            speed: Some("standard".into()),
            inference_geo: Some("global".into()),
        },
        TurnClass {
            model: Some("<synthetic>".into()),
            ..Default::default()
        },
    ];
    let mut turns: Vec<LoadedTurn> = l
        .turns
        .drain(..)
        .enumerate()
        .map(|(n, t)| {
            let ms = parse_ts_ms(&t.ts).unwrap();
            loaded(n as i64, (ms - at(1, 0)) / 1000, 0, 50_000)
        })
        .collect();
    let mut synthetic = loaded(99, 90, 1, 0);
    synthetic.cache_write = 0;
    turns.push(synthetic);
    l.sessions = vec![SessionTurns {
        session_id: "s".into(),
        turns,
    }];
    let line = &run(&l).ledger.recent[0];
    assert_eq!(
        (line.pair.rounds, line.pair.rounds_bound),
        (Some(4), Some("sessionEnd"))
    );
}

/// An open session's R is a lower bound: still measured, and the band's high end is then no
/// bound. The week here holds one repeat answer, which costs no round, so nothing else opens it.
#[test]
fn an_open_sessions_r_is_a_lower_bound_and_its_high_end_no_bound() {
    let mut l = derived_ledger();
    l.boundaries.clear();
    l.calls[0].ts = ts(MONDAY - 3_600_000);
    l.turns
        .push(turn("w0", "s", MONDAY - 3_600_000 + 9_000, SONNET));
    let mut stub = call(2, Some("s"), at(1, 5), ROUTE_UNCHANGED, "/r/a.rs", 11_970);
    stub.returned_tokens = 30;
    l.calls.push(stub);
    let s = run(&l);
    assert_eq!(s.week.counts.calls, 1);
    assert_eq!(s.week.tokens_saved.value, 2_000 - 30);
    assert!(!s.week.measured.unwrap().high_open, "{:?}", s.week);

    l.turns.push(turn("t6", "s", NOW - 600_000, SONNET));
    let s = run(&l);
    assert_eq!(s.week.counts.rounds_lower_bound_rows, 1);
    let measured = s.week.measured.unwrap();
    assert!(measured.high_open);
    assert_eq!(s.week.modelled, None, "a lower bound is still measured");
    assert!(text::tray_line(&s).unwrap().starts_with("Saved at least $"));
}

// ── what a call saved ────────────────────────────────────────────────────────

/// A fresh=true ask after a stub returns what the stub stood in for: what it returns is a loss,
/// not a saving against the whole file again.
#[test]
fn a_fresh_ask_after_a_stub_is_counted_against_nothing() {
    let mut l = derived_ledger();
    let mut stub = call(2, Some("s"), at(1, 65), ROUTE_UNCHANGED, "/r/a.rs", 11_970);
    stub.returned_tokens = 30;
    l.calls.push(stub);
    let mut fresh = call(3, Some("s"), at(1, 70), "smart_read", "/r/a.rs", 10_000);
    fresh.repeat_plan = Some(PLAN_FRESH.into());
    fresh.repeat_arm = Some(ARM_TREATMENT.into());
    l.calls.push(fresh.clone());
    let s = run(&l);
    let again = s
        .ledger
        .recent
        .iter()
        .find(|x| x.extra_round_basis == "freshAfterStub")
        .unwrap();
    assert_eq!(again.saved_against, "askedAgain");
    assert_eq!(again.saved_cl100k, -2_000);
    assert_eq!(again.tokens_saved.value, -2_000);
    // The file once (10,000), the stub (2,000 − 30), and the answer asked for again (−2,000):
    // not two files' worth.
    assert_eq!(s.week.tokens_saved.value, 10_000 + 1_970 - 2_000);

    // With no stub before it, a fresh=true call is a first read.
    let mut l = derived_ledger();
    l.calls.push(fresh);
    let s = run(&l);
    assert!(
        s.ledger
            .recent
            .iter()
            .all(|x| x.saved_against == "wholeFile")
    );
}

/// The built-in Read returns at most 25,000 tokens of a file: a larger file never reached the
/// context whole that way, so the saving is counted against what a Read returns.
#[test]
fn a_file_larger_than_a_read_returns_counts_as_what_a_read_returns() {
    let mut l = derived_ledger();
    l.calls[0].full_tokens = 100_000;
    l.calls[0].saved_tokens = 98_000;
    let s = run(&l);
    let line = &s.ledger.recent[0];
    assert!(line.read_capped);
    assert_eq!(line.recorded_saved_cl100k, 98_000);
    assert_eq!(line.saved_cl100k, READ_TOKEN_LIMIT - 2_000);
    assert_eq!(line.tokens_saved.value, READ_TOKEN_LIMIT - 2_000);
    assert_eq!(s.week.tokens_full.value, READ_TOKEN_LIMIT);
    assert!(s.method.counterfactual.contains("25,000"));
    // At the limit, nothing is cut.
    let line = &run(&derived_ledger()).ledger.recent[0];
    assert!(!line.read_capped);
    assert_eq!(line.saved_cl100k, 10_000);
}

// ── blocked Reads ────────────────────────────────────────────────────────────

fn fault(secs: i64, session: &str, path: &str, detail: Option<&str>) -> BlockedReadRow {
    BlockedReadRow {
        ts: ts(at(1, secs)),
        session_id: Some(session.into()),
        path: Some(path.into()),
        detail: detail.map(str::to_string),
        agent_id: None,
        version: None,
    }
}

/// The meter's row for the Read the valve let through, half a second after its fault, by the
/// main agent (`None`) or a subagent.
fn metered(secs: i64, session: &str, path: &str, agent: Option<&str>) -> ValveReadRow {
    ValveReadRow {
        session_id: session.into(),
        path: path.into(),
        ts_ms: Some(at(1, secs) + 500),
        agent_id: agent.map(str::to_string),
    }
}

/// The valve logs a fault on every Read of a path after its block, and one block costs one
/// round: a path is blocked again only after smart_read or recall_file answered for it, which
/// the fault says.
#[test]
fn repeated_valve_faults_on_one_path_are_one_round_and_a_second_only_on_evidence() {
    let mut l = derived_ledger();
    // The meter named agents from the start, so a Read it metered with none is the main agent's.
    l.meter_agents_from = Some(0);
    for secs in [61, 62, 63, 64] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
        l.valve_reads.push(metered(secs, "s", "/r/big.rs", None));
    }
    let s = run(&l);
    let blocked = |s: &SavingsSummary| {
        s.ledger
            .recent
            .iter()
            .filter(|x| x.kind == LineKind::BlockedRead)
            .count()
    };
    assert_eq!(blocked(&s), 1);
    assert_eq!(s.all_time.counts.blocked_reads, 1);
    assert_eq!(s.all_time.counts.valve_faults, 4);
    assert_eq!(s.all_time.counts.extra_rounds_evidenced, 1);
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    let call_net = run(&derived_ledger()).all_time.measured.unwrap().net.value;
    assert!(close(s.all_time.measured.unwrap().net.value, call_net - c));

    // Blocked again after a Lumen answer, and let through twice more: one more round.
    for secs in [130, 131] {
        l.blocked_reads
            .push(fault(secs, "s", "/r/big.rs", Some(AFTER_LUMEN_READ)));
        l.valve_reads.push(metered(secs, "s", "/r/big.rs", None));
    }
    // Another path is another block.
    l.blocked_reads.push(fault(140, "s", "/r/other.rs", None));
    l.valve_reads.push(metered(140, "s", "/r/other.rs", None));
    let s = run(&l);
    assert_eq!(blocked(&s), 3);
    assert_eq!(s.all_time.counts.valve_faults, 7);
    assert_eq!(s.all_time.counts.extra_rounds_evidenced, 3);
    assert_eq!(s.all_time.counts.blocked_reads_bounded, 0);
}

/// A subagent is blocked in its own right: the main agent's four Reads of a file (blocked, then
/// three faults) and a subagent's three (blocked, then two) are two blocks. From 1.9.0 the fault
/// names the subagent; the meter's row for the same Read says the others are the main agent's.
#[test]
fn a_subagents_block_on_a_file_is_counted_apart_from_its_parents() {
    let mut l = derived_ledger();
    l.meter_agents_from = Some(0);
    for secs in [61, 62, 63] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
        l.valve_reads.push(metered(secs, "s", "/r/big.rs", None));
    }
    for secs in [70, 71] {
        let mut f = fault(secs, "s", "/r/big.rs", None);
        f.agent_id = Some("a9".into());
        l.blocked_reads.push(f);
        l.valve_reads
            .push(metered(secs, "s", "/r/big.rs", Some("a9")));
    }
    let s = run(&l);
    let c = &s.all_time.counts;
    assert_eq!((c.blocked_reads, c.valve_faults), (2, 5));
    assert_eq!(c.blocked_reads_bounded, 0);
    assert_eq!(c.extra_rounds_evidenced, 2);

    // A fault from before 1.9.0 names no agent: the meter's row for its Read does.
    for f in l.blocked_reads.iter_mut() {
        f.agent_id = None;
    }
    let s = run(&l);
    assert_eq!(s.all_time.counts.blocked_reads, 2);

    // The meter named no agent before 1.8.0: a row from then says nothing of whose Read it was,
    // and neither does a fault with no row. The five faults are then at most one block per agent
    // that could have read the file: the main agent and a9, whose transcript has turns then.
    l.meter_agents_from = Some(at(9, 0));
    for r in l.valve_reads.iter_mut() {
        r.agent_id = None;
    }
    let sub = |id: &str, secs: i64| TurnRow {
        is_subagent: true,
        source_file: Some("/h/.claude/projects/-p/s/subagents/agent-a9.jsonl".into()),
        ..turn(id, "s", at(1, secs), SONNET)
    };
    l.turns.push(sub("a9-1", 66));
    let s = run(&l);
    let c = &s.all_time.counts;
    assert_eq!((c.blocked_reads, c.blocked_reads_bounded), (2, 1));
    assert_eq!((c.extra_rounds_evidenced, c.valve_faults), (1, 5));
    let bounded = s
        .ledger
        .recent
        .iter()
        .filter(|x| x.extra_round_basis == "blockedReadBounded")
        .count();
    assert_eq!(bounded, 1);
    l.valve_reads.clear();
    assert_eq!(run(&l).all_time.counts.blocked_reads, 2);
}

/// The meter's row for a Read is matched to its fault from a second before the fault's recorded
/// second to three after it, and no further: a row outside says nothing of that fault.
#[test]
fn the_meters_row_is_matched_from_a_second_before_the_fault_to_three_after() {
    let bounded = |offset_ms: i64| {
        let mut l = derived_ledger();
        l.meter_agents_from = Some(0);
        // The main agent's fault, attributed by its own row.
        l.blocked_reads.push(fault(61, "s", "/r/big.rs", None));
        l.valve_reads.push(metered(61, "s", "/r/big.rs", None));
        // A second fault on the file, and a5's row for it `offset_ms` from the fault's second.
        l.blocked_reads.push(fault(120, "s", "/r/big.rs", None));
        let mut row = metered(120, "s", "/r/big.rs", Some("a5"));
        row.ts_ms = Some(at(1, 120) + offset_ms);
        l.valve_reads.push(row);
        let c = run(&l).all_time.counts;
        assert_eq!(c.blocked_reads, 2, "main and a5 either way");
        c.blocked_reads_bounded
    };
    // Matched: a5's block is on evidence.
    assert_eq!(bounded(-1_000), 0);
    assert_eq!(bounded(3_000), 0);
    // Not matched: the fault is no one's on evidence, and a5, named on the file then, bounds it.
    assert_eq!(bounded(-1_001), 1);
    assert_eq!(bounded(3_001), 1);
}

/// A hook from 1.9.0 names the subagent on every fault it writes, so a fault it wrote that names
/// none is the main agent's: on evidence, not bounded by the subagents running then.
#[test]
fn a_fault_from_1_9_0_that_names_no_agent_is_the_main_agents() {
    let mut l = derived_ledger();
    for secs in [61, 62, 63] {
        let mut f = fault(secs, "s", "/r/big.rs", None);
        f.version = Some("1.9.0".into());
        l.blocked_reads.push(f);
    }
    // Two subagents running then.
    for agent in ["x", "y"] {
        l.turns.push(TurnRow {
            is_subagent: true,
            source_file: Some(format!(
                "/h/.claude/projects/-p/s/subagents/agent-{agent}.jsonl"
            )),
            ..turn(&format!("{agent}1"), "s", at(1, 62), SONNET)
        });
    }
    let counts = |l: &Ledger| {
        let c = run(l).all_time.counts;
        (
            c.blocked_reads,
            c.blocked_reads_bounded,
            c.extra_rounds_evidenced,
        )
    };
    assert_eq!(counts(&l), (1, 0, 1));
    for version in ["1.10.2", "2.0.0", "1.9.0-rc.1"] {
        for f in l.blocked_reads.iter_mut() {
            f.version = Some(version.into());
        }
        assert_eq!(counts(&l), (1, 0, 1), "{version}");
    }
    // From 1.8.0 or with no version, nothing says whose: up to one per agent running then.
    for version in [Some("1.8.0"), None, Some("dev")] {
        for f in l.blocked_reads.iter_mut() {
            f.version = version.map(str::to_string);
        }
        assert_eq!(counts(&l), (3, 2, 1), "{version:?}");
    }
}

/// Faults no evidence attributes, on one file: a later fault is an allowed Read, and a new block
/// can only be another agent's. So they stand for at most one block per agent that could have
/// read the file then, never more than there are faults.
#[test]
fn faults_no_agent_is_known_for_are_bounded_by_the_agents_running_then() {
    let mut l = derived_ledger();
    // Five faults on one file between 1:01 and 1:05, none attributed.
    for secs in [61, 62, 63, 64, 65] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
    }
    let blocks = |l: &Ledger| {
        let c = run(l).all_time.counts;
        (c.blocked_reads, c.blocked_reads_bounded, c.valve_faults)
    };
    // No subagent in the session: the main agent's block alone, however many faults.
    assert_eq!(blocks(&l), (1, 0, 5));
    // Two subagents with turns in the window: at most three.
    let sub = |agent: &str, ms: i64| TurnRow {
        is_subagent: true,
        source_file: Some(format!(
            "/h/.claude/projects/-p/s/subagents/agent-{agent}.jsonl"
        )),
        ..turn(&format!("{agent}-{ms}"), "s", ms, SONNET)
    };
    l.turns.push(sub("x", at(1, 61) - 4 * 60_000));
    l.turns.push(sub("y", at(1, 65) + 4 * 60_000));
    assert_eq!(blocks(&l), (3, 2, 5));
    // A subagent running only outside the five minutes either side is not one of them.
    l.turns.push(sub("z", at(1, 61) - 6 * 60_000));
    l.turns.push(sub("z", at(1, 65) + 6 * 60_000));
    assert_eq!(blocks(&l), (3, 2, 5));
    // An agent the meter named on that file then is one, even with no turn in the ledger.
    l.meter_agents_from = Some(0);
    l.valve_reads
        .push(metered(200, "s", "/r/big.rs", Some("w")));
    assert_eq!(blocks(&l), (4, 3, 5));
    // Never more blocks than faults.
    l.blocked_reads.truncate(2);
    assert_eq!(blocks(&l), (2, 1, 2));
}

/// No Lumen call in a week of blocked Reads: every block was a round spent for nothing.
#[test]
fn blocked_reads_with_no_lumen_call_say_the_intercept_costs_and_saves_nothing() {
    let mut l = derived_ledger();
    l.calls.clear();
    l.recent_turns = 6;
    l.meter_agents_from = Some(0);
    for secs in [61, 62, 63] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
        l.valve_reads.push(metered(secs, "s", "/r/big.rs", None));
    }
    l.blocked_reads.push(fault(121, "s", "/r/other.rs", None));
    l.valve_reads.push(metered(121, "s", "/r/other.rs", None));
    let s = run(&l);
    let Status::NotWorking { reason } = &s.status else {
        panic!("{:?}", s.status)
    };
    assert!(
        reason.contains("blocked 2 reads (4 escape-valve faults)"),
        "{reason}"
    );
    let r = s.recommendation.clone().unwrap();
    assert_eq!(r.code, "blocksWithoutSavings");
    // Two rounds of t2 and t3 as they ran, $0.03378 each.
    assert!(r.text.contains("about $0.07 in extra rounds"), "{}", r.text);
    assert!(r.text.contains("LUMEN_HOOK_ENABLED=0"));
    // The tray and the one-liner say what the figure is: blocked Reads alone.
    let tray = text::tray_line(&s).unwrap();
    assert!(
        tray.starts_with("Net loss of at most $0.07 this week (2 blocked Reads) \u{b7} "),
        "{tray}"
    );
    assert!(text::headline_line(&s).contains("(2 blocked Reads)"));

    // With no evidence of whose Reads they were and a subagent running then, up to one more:
    // the cost is an upper bound.
    l.valve_reads.clear();
    l.turns.push(TurnRow {
        is_subagent: true,
        source_file: Some("/h/.claude/projects/-p/s/subagents/agent-x.jsonl".into()),
        ..turn("x1", "s", at(1, 50), SONNET)
    });
    let s = run(&l);
    let Status::NotWorking { reason } = &s.status else {
        panic!("{:?}", s.status)
    };
    assert!(
        reason.contains("blocked 2 reads, and up to 1 more where no agent is known"),
        "{reason}"
    );
    assert!(reason.contains("up to $0.10 in extra rounds"), "{reason}");
}

// ── measured, modelled, tokens only ──────────────────────────────────────────

/// One of each: calls measured from their own turns with a blocked Read, a call whose R may be
/// too long, a call with no session and one read by a model with no price. The bold figure is the
/// measured part alone; the modelled dollars and the unpriced calls are said beside it.
#[test]
fn the_bold_figure_is_the_measured_part_alone_and_the_rest_is_said_beside_it() {
    let mut l = derived_ledger();
    // Events recorded from 03:00: session s is vouched for by its compaction, u by nothing.
    l.events_from = Some(at(3, 0));
    l.blocked_reads.push(fault(61, "s", "/r/big.rs", None));
    for (id, secs) in [("u0", 0), ("u1", 9), ("u2", 60), ("u3", 120)] {
        l.turns.push(turn(id, "u", at(2, secs), SONNET));
    }
    l.calls
        .push(call(2, Some("u"), at(2, 5), "smart_read", "/r/u.rs", 6_000));
    l.calls
        .push(call(3, None, at(4, 0), "smart_read", "/r/none.rs", 5_000));
    l.calls
        .push(call(4, Some("h"), at(5, 0), "smart_read", "/r/h.rs", 4_000));
    l.turns.push(turn("h1", "h", at(5, 3), "claude-haiku-5-5"));
    let s = run(&l);
    let net_of = |dollars: &str| -> Vec<f64> {
        s.ledger
            .recent
            .iter()
            .filter(|x| x.dollars == dollars)
            .map(|x| x.net_usd.unwrap().value)
            .collect()
    };
    let measured_lines = net_of("measured");
    assert_eq!(
        measured_lines.len(),
        3,
        "s's call, its blocked Read, and u's call at R = 0"
    );
    let rest: Vec<Band<f64>> = s
        .ledger
        .recent
        .iter()
        .filter_map(|x| x.modelled_value_usd)
        .collect();
    assert_eq!(
        rest.len(),
        1,
        "u's call alone has an R that may be too long"
    );
    let w = &s.week;
    let measured = w.measured.unwrap();
    let modelled = w.modelled.unwrap();
    assert_eq!((measured.rows, modelled.rows), (3, 1));
    assert!(close(
        measured.net.value,
        measured_lines.iter().sum::<f64>()
    ));
    assert!(close(modelled.value.value, rest[0].value));
    assert!(
        modelled.value.value > 1e-6,
        "premise: adding it would change the figure"
    );
    // Tokens only: counted in the tokens, in no dollar figure.
    let u = w.unpriced;
    assert_eq!(
        (u.calls, u.no_session, u.no_price, u.tokens.value),
        (2, 1, 1, 9_000)
    );
    assert_eq!(w.tokens_saved.value, 10_000 + 6_000 + 5_000 + 4_000);

    // The figure is the measured net, a loss here: the most it can be.
    let phrase = text::usd_phrase(measured.net.value);
    assert!(phrase.starts_with("net loss of at most $"), "{phrase}");
    let beside = format!(
        "({} modelled; 2 calls unpriced)",
        text::usd_plus(modelled.value.value)
    );
    let tray = text::tray_line(&s).unwrap();
    assert!(
        tray.starts_with(&format!("N{} this week {beside} \u{b7} ", &phrase[1..])),
        "{tray}"
    );
    let headline = text::headline_line(&s);
    assert!(
        headline.contains(&format!("; {phrase} {beside} \u{b7} all time: ")),
        "{headline}"
    );
    assert!(!headline.contains('\n'));
    // Nothing beside a figure that leaves nothing out.
    let s = run(&derived_ledger());
    assert!(s.week.modelled.is_none() && s.week.unpriced.calls == 0);
    assert!(!text::tray_line(&s).unwrap().contains('('));
}

/// A sign is the value's, never the rounding's: a loss under half a cent is a loss.
#[test]
fn a_sign_is_never_lost_to_rounding() {
    assert_eq!(text::usd_signed(-0.0046), "\u{2212}$0.00");
    assert_eq!(text::usd_plus(-0.0046), "\u{2212}$0.00");
    assert_eq!(text::usd_signed(0.004), "$0.00");
    assert_eq!(text::usd_plus(0.004), "+$0.00");
    // Only zero itself has no sign.
    assert_eq!(text::usd_signed(0.0), "$0.00");
    assert_eq!(text::usd_signed(-0.0), "$0.00");
    assert_eq!(text::usd_plus(0.0), "$0.00");
    assert_eq!(text::usd_signed(-123.4), "\u{2212}$123");
    assert_eq!(text::usd_plus(1.5), "+$1.50");
}

/// A loss every end of whose band is a loss is one Lumen did cause: inline text for
/// compress_logs saves nothing and pays a round on evidence.
#[test]
fn a_loss_whose_band_is_a_loss_throughout_says_lumen_cost_more() {
    let mut l = derived_ledger();
    let c = &mut l.calls[0];
    c.tool = "mcp__lumen__compress_logs".into();
    c.routed_via = "compress_logs".into();
    c.path = INLINE_PATH.into();
    let s = run(&l);
    let d = s.week.measured.unwrap();
    assert!(d.net.high <= 0.0 && !d.high_open, "premise: {d:?}");
    assert_eq!(
        s.status,
        Status::NotWorking {
            reason: format!(
                "Lumen cost more than it saved: {} this week",
                text::usd_phrase(d.net.value)
            )
        }
    );

    // The same loss in a session that may still be running (no compaction ends its R, and a
    // turn came ten minutes ago): its high end is no bound, so the loss is not certain.
    l.boundaries.clear();
    l.turns.push(turn("t6", "s", NOW - 600_000, SONNET));
    let s = run(&l);
    let d = s.week.measured.unwrap();
    assert!(d.net.high <= 0.0 && d.high_open, "premise: {d:?}");
    assert_eq!(
        s.status,
        Status::NotWorking {
            reason: format!(
                "Lumen may have cost more than it saved: {} this week",
                text::usd_phrase(d.net.value)
            )
        }
    );
}

/// A dollar figure that is not a number is left out, said, and never printed.
#[test]
fn a_figure_that_is_not_a_number_is_never_shown_as_one() {
    let mut s = run(&derived_ledger());
    let mut d = s.week.measured.unwrap();
    d.net.value = f64::NAN;
    s.week.measured = Some(d);
    s.week.modelled = Some(Modelled {
        rows: 1,
        value: Band::exact(f64::INFINITY),
    });
    s.week.share_pct = Some(f64::NAN);
    let p = finite_only(s.week.clone());
    assert_eq!((p.measured, p.modelled, p.share_pct), (None, None, None));
    assert_eq!(
        p.data_quality,
        DataQuality::Partial {
            reason: NOT_A_NUMBER.to_string()
        }
    );
    // And the words never print one, whatever reaches them.
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(text::usd_signed(x), "no figure");
        assert_eq!(text::usd_phrase(x), "no figure");
        assert_eq!(text::share_text(x), "no figure");
    }
    assert!(!text::headline_line(&s).contains("NaN"));
}

// ── blocked Reads the transcripts record ─────────────────────────────────────

/// A block the transcript of session `s` records, `secs` past 1:00.
fn recorded(secs: i64, agent: Option<&str>, path: &str, kind: &str) -> RecordedBlockRow {
    RecordedBlockRow {
        session_id: "s".into(),
        agent_id: agent.map(str::to_string),
        path: path.into(),
        ts_ms: at(1, secs),
        kind: kind.into(),
    }
}

/// `agent`'s own transcript in session `s`, read for blocks through `through_ms`.
fn scanned(agent: Option<&str>, through_ms: i64) -> ScannedRow {
    ScannedRow {
        source_file: match agent {
            Some(a) => format!("/h/.claude/projects/-p/s/subagents/agent-{a}.jsonl"),
            None => "/h/.claude/projects/-p/s.jsonl".into(),
        },
        session_id: "s".into(),
        agent_id: agent.map(str::to_string),
        through_ms,
    }
}

/// A turn of subagent `agent` of session `s`, at `ms`.
fn sub_turn(agent: &str, ms: i64, model: &str) -> TurnRow {
    TurnRow {
        is_subagent: true,
        source_file: Some(format!(
            "/h/.claude/projects/-p/s/subagents/agent-{agent}.jsonl"
        )),
        ..turn(&format!("{agent}-{ms}"), "s", ms, model)
    }
}

/// The blocked Reads of all time: in all, from transcripts, from faults on evidence, bounded,
/// and the escape-valve faults behind them.
fn blocks_of(l: &Ledger) -> (i64, i64, i64, i64, i64) {
    let c = run(l).all_time.counts;
    (
        c.blocked_reads,
        c.blocked_reads_from_transcripts,
        c.blocked_reads_from_faults,
        c.blocked_reads_bounded,
        c.valve_faults,
    )
}

/// A block the transcript records is one round, on evidence, priced at the turn of the agent
/// that tried the Read that read the block's message: the main agent's next turn, or the
/// subagent's own.
#[test]
fn a_block_a_transcript_records_is_one_round_priced_at_the_turn_after_it() {
    let mut l = derived_ledger();
    l.recorded_blocks
        .push(recorded(30, None, "/r/big.rs", "redirect"));
    let s = run(&l);
    assert_eq!(blocks_of(&l), (1, 1, 0, 0, 0));
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.kind == LineKind::BlockedRead)
        .unwrap();
    assert_eq!(line.extra_round_basis, "blockedReadRecorded");
    assert!(line.extra_round_evidenced);
    assert_eq!(
        (line.ts.as_str(), line.path.as_deref()),
        (ts(at(1, 30)).as_str(), Some("/r/big.rs"))
    );
    // t2, at 1:01, read the block's message: its round as it ran.
    let c = (10.0 * 3.0 + 500.0 * 15.0 + 50_000.0 * 0.3 + 3_000.0 * 3.75) / 1e6;
    assert!(close(line.extra_round_usd.unwrap().value, c), "{line:?}");
    let call_net = run(&derived_ledger()).all_time.measured.unwrap().net.value;
    assert!(close(s.all_time.measured.unwrap().net.value, call_net - c));

    // A subagent's block is priced at the subagent's own next turn, not the main agent's.
    l.turns.push(sub_turn("x", at(1, 40), OPUS));
    l.turns.push(sub_turn("x", at(1, 70), OPUS));
    l.recorded_blocks
        .push(recorded(45, Some("x"), "/r/big.rs", "redirect"));
    let s = run(&l);
    assert_eq!(blocks_of(&l), (2, 2, 0, 0, 0));
    let line = s
        .ledger
        .recent
        .iter()
        .find(|x| x.kind == LineKind::BlockedRead && x.ts == ts(at(1, 45)))
        .unwrap();
    assert_eq!(line.pair.model.as_deref(), Some(OPUS));
    let c = (10.0 * 5.0 + 500.0 * 25.0 + 50_000.0 * 0.5 + 3_000.0 * 6.25) / 1e6;
    assert!(close(line.extra_round_usd.unwrap().value, c), "{line:?}");

    // Each block a transcript records is counted, a path blocked twice in one context included
    // (the intercept's marker file was cleared in between).
    l.recorded_blocks
        .push(recorded(130, None, "/r/big.rs", "redirect"));
    assert_eq!(blocks_of(&l), (3, 3, 0, 0, 0));
    // One whose agent has no turn after it has no round to price: tokens only, still counted.
    l.recorded_blocks
        .push(recorded(300, None, "/r/big.rs", "redirect"));
    let s = run(&l);
    assert_eq!(s.all_time.counts.blocked_reads, 4);
    assert_eq!(s.all_time.unpriced.blocked_reads, 1);
}

/// Where a transcript records the block on a path, the escape-valve faults on it are its later
/// Reads: corroboration, not more blocks. Faults no evidence attributes are no longer bounded by
/// an agent whose transcript was read through its latest turn and holds no block of the path.
#[test]
fn recorded_blocks_are_the_evidence_and_the_faults_on_their_path_corroborate() {
    let mut l = derived_ledger();
    // Five faults on one file from a hook before 1.9.0, with no meter row: nothing says whose.
    for secs in [61, 62, 63, 64, 65] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
    }
    // Two subagents running then.
    let x_last = at(1, 61) - 4 * 60_000;
    let y_last = at(1, 65) + 4 * 60_000;
    l.turns.push(sub_turn("x", x_last, SONNET));
    l.turns.push(sub_turn("y", y_last, SONNET));
    assert_eq!(blocks_of(&l), (3, 0, 1, 2, 5), "the rule for faults alone");

    // The main agent's transcript records the block, and the three transcripts were read
    // through their latest turns.
    l.recorded_blocks
        .push(recorded(50, None, "/r/big.rs", "redirect"));
    l.scanned.push(scanned(None, at(1, 240)));
    l.scanned.push(scanned(Some("x"), x_last));
    l.scanned.push(scanned(Some("y"), y_last));
    assert_eq!(blocks_of(&l), (1, 1, 0, 0, 5));
    let line = run(&l)
        .ledger
        .recent
        .into_iter()
        .find(|x| x.kind == LineKind::BlockedRead)
        .unwrap();
    assert_eq!(line.extra_round_basis, "blockedReadRecorded");

    // y's transcript not read: y may have been blocked on the file too, at most once.
    l.scanned.retain(|r| r.agent_id.as_deref() != Some("y"));
    assert_eq!(blocks_of(&l), (2, 1, 0, 1, 5));
    // x's read short of its latest turn: it may hold a block not read yet.
    l.scanned[1].through_ms = x_last - 1;
    assert_eq!(blocks_of(&l), (3, 1, 0, 2, 5));
    l.scanned[1].through_ms = x_last;
    l.scanned.push(scanned(Some("y"), y_last));

    // A fault from 1.9.0 names its agent: x's, when x's transcript records the block, is its
    // later Read; z's, with no block of the file recorded, is z's block, on the fault's evidence.
    for agent in ["x", "z"] {
        let mut f = fault(70, "s", "/r/big.rs", None);
        f.agent_id = Some(agent.into());
        f.version = Some("1.9.0".into());
        l.blocked_reads.push(f);
    }
    l.recorded_blocks
        .push(recorded(66, Some("x"), "/r/big.rs", "redirect"));
    assert_eq!(blocks_of(&l), (3, 2, 1, 0, 7));
}

/// A path with faults and no block a transcript records keeps the rule for faults, whatever the
/// transcripts record of other paths: a transcript read through and showing no block of a path
/// that has faults is a matcher that missed one as likely as a block that never was.
#[test]
fn a_path_only_faults_speak_for_keeps_the_rule_for_faults() {
    let mut l = derived_ledger();
    for secs in [61, 62, 63, 64, 65] {
        l.blocked_reads.push(fault(secs, "s", "/r/big.rs", None));
    }
    let x_last = at(1, 61) - 4 * 60_000;
    let y_last = at(1, 65) + 4 * 60_000;
    l.turns.push(sub_turn("x", x_last, SONNET));
    l.turns.push(sub_turn("y", y_last, SONNET));
    l.recorded_blocks
        .push(recorded(50, None, "/r/other.rs", "redirect"));
    l.scanned.push(scanned(None, at(1, 240)));
    l.scanned.push(scanned(Some("x"), x_last));
    l.scanned.push(scanned(Some("y"), y_last));
    assert_eq!(blocks_of(&l), (4, 1, 1, 2, 5));

    // Blocked again after a Lumen answer is a block of its own kind: a recorded first block of
    // the path does not stand for it.
    let mut l = derived_ledger();
    l.blocked_reads
        .push(fault(90, "s", "/r/big.rs", Some(AFTER_LUMEN_READ)));
    l.recorded_blocks
        .push(recorded(50, None, "/r/big.rs", "redirect"));
    l.scanned.push(scanned(None, at(1, 240)));
    assert_eq!(blocks_of(&l), (2, 1, 1, 0, 1));
    l.recorded_blocks
        .push(recorded(80, None, "/r/big.rs", "served"));
    assert_eq!(blocks_of(&l), (2, 2, 0, 0, 1));
}

/// A call may have followed a blocked Read. Where a transcript records a block of the call's own
/// file in the call's own conversation, before it, and was read through the call, that block is
/// the call's round, on a line of its own, and the call carries none. One block answers one call.
#[test]
fn a_call_after_a_recorded_block_of_its_file_carries_no_round_of_its_own() {
    let mut l = derived_ledger();
    let rounds = |l: &Ledger| {
        let s = run(l);
        let call = s
            .ledger
            .recent
            .iter()
            .find(|x| x.kind == LineKind::Call)
            .unwrap()
            .clone();
        let c = &s.all_time.counts;
        (
            call.extra_round_basis,
            call.extra_rounds,
            c.extra_rounds_evidenced,
            c.extra_rounds_modelled,
        )
    };
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 0, 1));

    // The block of its file before it, recorded, and the transcript read through the call: one
    // round in all, the block's.
    l.recorded_blocks
        .push(recorded(3, None, "/r/a.rs", "redirect"));
    l.scanned.push(scanned(None, at(1, 240)));
    assert_eq!(rounds(&l), ("blockRecorded", 0, 1, 0));

    // A block of another file is its own round, and not this call's.
    l.recorded_blocks[0].path = "/r/zzz.rs".into();
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    l.recorded_blocks[0].path = "/r/a.rs".into();
    // Nor one after the call, in the call's own second included.
    l.recorded_blocks[0].ts_ms = at(1, 5) + 500;
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    l.recorded_blocks[0].ts_ms = at(1, 3);

    // A transcript not read through the call yet may hold another block before it.
    l.scanned[0].through_ms = at(1, 4);
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    l.scanned[0].through_ms = at(1, 240);
    // Another file read with the session's lines (here an older Claude Code's subagent
    // transcript beside the session's) does not vouch for the conversation on its own...
    l.scanned[0].source_file = "/h/.claude/projects/-p/agent-a7.jsonl".into();
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    // ...and holds the conversation's own reading down to its own.
    l.scanned[0] = scanned(None, at(1, 240));
    l.scanned.push(ScannedRow {
        source_file: "/h/.claude/projects/-p/agent-a7.jsonl".into(),
        through_ms: at(1, 4),
        ..scanned(None, 0)
    });
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    l.scanned.pop();
    assert_eq!(rounds(&l), ("blockRecorded", 0, 1, 0));

    // A subagent's call is answered by its own block, in its own transcript.
    l.calls[0].agent_id = Some("x".into());
    l.turns.push(sub_turn("x", at(1, 9), SONNET));
    assert_eq!(rounds(&l), ("blockNotRecorded", 1, 1, 1));
    l.recorded_blocks
        .push(recorded(3, Some("x"), "/r/a.rs", "redirect"));
    l.scanned.push(scanned(Some("x"), at(1, 240)));
    assert_eq!(rounds(&l), ("blockRecorded", 0, 2, 0));
    l.calls[0].agent_id = None;

    // One block answers one call: a second outline of the file later may have followed a block
    // of its own.
    l.calls.push(call(
        2,
        Some("s"),
        at(1, 100),
        "smart_read",
        "/r/a.rs",
        10_000,
    ));
    let c = run(&l).all_time.counts;
    assert_eq!((c.extra_rounds_evidenced, c.extra_rounds_modelled), (2, 1));
}

/// Both loaders read the blocks the transcripts record, with the turns of the sessions they name,
/// and the transcripts read for blocks of the sessions the summary reads.
#[test]
fn the_rusqlite_loader_reads_recorded_blocks_and_the_transcripts_read_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    let conn = crate::meter::connect_db(&db).unwrap();
    conn.execute_batch(
        "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                           cache_read_input_tokens,cache_creation_input_tokens,cache_5m,cache_1h,
                           speed,inference_geo)
         VALUES('t1','b','2026-10-12T01:00:09.250Z','claude-sonnet-4-6',10,500,50000,3000,3000,0,
                'standard','global');
         INSERT INTO intercept_blocks(tool_use_id,session_id,agent_id,path,lines,kind,ts_ms,uuid,
                                      source_file)
         VALUES('toolu_1','b',NULL,'/r/big.rs',400,'redirect',1791766805000,'r-1',
                '/h/.claude/projects/-p/b.jsonl'),
               ('toolu_2','b','a7','/r/big.rs',400,'served',1791766806000,'r-2',
                '/h/.claude/projects/-p/b/subagents/agent-a7.jsonl');
         INSERT INTO transcript_scans(source_file,session_id,agent_id,matcher,through_ms)
         VALUES('/h/.claude/projects/-p/b.jsonl','b',NULL,1,1791766809250),
               ('/h/.claude/projects/-p/q.jsonl','q',NULL,1,1791766809250);",
    )
    .unwrap();
    let (ledger, _) = load::load(&conn, NOW, None).unwrap();
    assert_eq!(
        ledger.recorded_blocks,
        [
            RecordedBlockRow {
                session_id: "b".into(),
                agent_id: None,
                path: "/r/big.rs".into(),
                ts_ms: 1_791_766_805_000,
                kind: "redirect".into(),
            },
            RecordedBlockRow {
                session_id: "b".into(),
                agent_id: Some("a7".into()),
                path: "/r/big.rs".into(),
                ts_ms: 1_791_766_806_000,
                kind: "served".into(),
            },
        ]
    );
    assert_eq!(ledger.sessions.len(), 1, "the session a block names");
    assert_eq!(
        ledger.scanned,
        [ScannedRow {
            source_file: "/h/.claude/projects/-p/b.jsonl".into(),
            session_id: "b".into(),
            agent_id: None,
            through_ms: 1_791_766_809_250,
        }],
        "only the sessions the summary reads"
    );
    let s = summarize(&ledger, &clock(), &facts(), &Uncalibrated);
    assert_eq!(s.all_time.counts.blocked_reads_from_transcripts, 2);
    // The main agent's block is priced at t1; a7 has no turn in the ledger.
    assert_eq!(s.all_time.measured.map(|m| m.rows), Some(1));
    assert_eq!(s.all_time.unpriced.blocked_reads, 1);
}

/// Two blocks, each answered with smart_read; a transcript records only the first (the other was
/// lost to the daemon's race), and the transcript is read through both calls. Two rounds were
/// spent, and a block of one file stands for no other file's.
#[test]
fn a_block_recorded_on_another_file_does_not_stand_for_a_calls_round() {
    let mut l = derived_ledger();
    l.calls.push(call(
        2,
        Some("s"),
        at(1, 2),
        "smart_read",
        "/r/other.rs",
        10_000,
    ));
    l.calls.sort_by_key(|c| c.ts.clone());
    l.recorded_blocks
        .push(recorded(1, None, "/r/other.rs", "redirect"));
    l.scanned.push(scanned(None, at(1, 240)));
    let c = run(&l).all_time.counts;
    assert_eq!(
        (c.extra_rounds_evidenced, c.extra_rounds_modelled),
        (1, 1),
        "the recorded block, and the call on /r/a.rs that may have followed one"
    );
}

/// Blocked Reads say where they come from only where they are not all of one kind on evidence:
/// the same words as savings-text.ts's `blockedReadsText`.
#[test]
fn blocked_reads_say_where_they_come_from_only_where_they_are_mixed() {
    let c = |t: i64, f: i64, b: i64| Counts {
        blocked_reads: t + f + b,
        blocked_reads_from_transcripts: t,
        blocked_reads_from_faults: f,
        blocked_reads_bounded: b,
        ..Counts::default()
    };
    assert_eq!(text::blocked_reads_text(&c(0, 0, 0)), None);
    assert_eq!(
        text::blocked_reads_text(&c(1, 0, 0)).unwrap(),
        "1 blocked Read"
    );
    assert_eq!(
        text::blocked_reads_text(&c(1200, 0, 0)).unwrap(),
        "1,200 blocked Reads"
    );
    assert_eq!(
        text::blocked_reads_text(&c(0, 9, 0)).unwrap(),
        "9 blocked Reads"
    );
    assert_eq!(
        text::blocked_reads_text(&c(3, 9, 0)).unwrap(),
        "12 blocked Reads: 3 recorded, 9 from faults"
    );
    assert_eq!(
        text::blocked_reads_text(&c(0, 9, 2)).unwrap(),
        "11 blocked Reads: 9 from faults, 2 bounded"
    );
}
