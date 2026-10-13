//! The frontend's `SavingsSummary` type names every field the engine sends.
//!
//! The app renders the engine's summary and computes nothing, so a field the engine adds or
//! renames and the type does not would be read as `undefined` and shown as nothing, silently.
//! This serializes a summary with every optional part present and looks for each key, camelCase,
//! in `lumenator/src/app/savings-summary.ts`.

use std::collections::BTreeSet;

use lumen_core::savings::{
    BillingView, BoundaryRow, CallRow, Clock, Facts, Ledger, TurnRow, Uncalibrated, iso_ms,
    summarize,
};

const MONDAY: i64 = 1_791_763_200_000;

fn turn(id: &str, ms: i64) -> TurnRow {
    TurnRow {
        message_id: id.into(),
        session_id: "s".into(),
        ts: iso_ms(ms),
        model: Some("claude-sonnet-4-6".into()),
        speed: Some("standard".into()),
        inference_geo: Some("global".into()),
        input: 10,
        output: 900,
        cache_read: 150_000,
        cache_write: 3_000,
        cache_5m: Some(3_000),
        cache_1h: Some(0),
        is_subagent: false,
        source_file: Some("/h/.claude/projects/-p/s.jsonl".into()),
    }
}

fn keys(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.insert(k.clone());
                keys(v, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
        _ => {}
    }
}

#[test]
fn every_field_the_engine_sends_is_in_the_frontends_type() {
    let at = |s: i64| MONDAY + 3_600_000 + s * 1000;
    // A small saving read by a large context, cut short by a compaction: a losing week, so the
    // recommendation is present too.
    let ledger = Ledger {
        calls: vec![CallRow {
            rowid: 1,
            ts: iso_ms(at(5)),
            tool: "mcp__lumen__smart_read".into(),
            path: "/r/a.rs".into(),
            routed_via: "smart_read".into(),
            full_tokens: 2_300,
            returned_tokens: 2_000,
            saved_tokens: 300,
            session_id: Some("s".into()),
            agent_id: None,
            req_key: None,
            token_source: Some("measured".into()),
            repeat_arm: None,
            repeat_plan: None,
        }],
        turns: vec![turn("t0", at(0)), turn("t1", at(9)), turn("t2", at(60))],
        boundaries: vec![BoundaryRow {
            session_id: "s".into(),
            kind: "compaction".into(),
            ts_ms: at(90),
            source_file: None,
        }],
        ..Default::default()
    };
    let s = summarize(
        &ledger,
        &Clock {
            now_ms: MONDAY + 86_400_000,
            week_start_ms: MONDAY,
            week_start: "2026-10-12".into(),
        },
        &Facts {
            billing_mode: BillingView::Api,
            identity_hook_registered: Some(true),
        },
        &Uncalibrated,
    );
    assert!(
        s.recommendation.is_some(),
        "premise: a recommendation is sent"
    );
    assert!(
        !s.ledger.top_losses.is_empty(),
        "premise: a call line is sent"
    );

    let mut sent = BTreeSet::new();
    keys(&serde_json::to_value(&s).unwrap(), &mut sent);
    let ts = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../lumenator/src/app/savings-summary.ts"),
    )
    .unwrap();
    let missing: Vec<&String> = sent
        .iter()
        .filter(|k| !ts.contains(&format!("{k}:")) && !ts.contains(&format!("{k}?:")))
        .collect();
    assert!(
        missing.is_empty(),
        "savings-summary.ts lacks fields the engine sends: {missing:?}"
    );
}
