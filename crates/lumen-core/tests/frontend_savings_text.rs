//! The terminal, the tray and the app say the same figures, signs and labels.
//!
//! `lumen_core::savings::text` makes bare `lumen`'s line and the tray's; the app's headline is
//! made by `lumenator/src/app/savings-text.ts`, phrase for phrase. Both are tested against one
//! case list, `lumenator/src/app/savings-text.cases.json`: the numbers each formatter must give,
//! and summaries in every state the headline has (API, subscription, not set, unreadable, a loss,
//! a loss under half a cent, partial data, modelled and unpriced dollars, a week of blocked Reads
//! alone, an empty week, an empty ledger, the ties Rust and JavaScript round differently, and
//! thousands, where the gross is far from the net). A change to one side that the other does not
//! make fails one of them.

use std::path::{Path, PathBuf};

use lumen_core::savings::{
    Band, BillingView, Calibrated, Clock, DataQuality, Dollars, Facts, Ledger, Modelled, Period,
    Recommendation, SavingsSummary, Status, summarize, text,
};
use serde_json::Value;

fn workspace_root() -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("crates").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The case list. Outside the workspace (a crate built on its own) there is none to read; inside
/// it, a missing or unreadable list fails, so the parity cannot pass by reading nothing.
fn cases() -> Option<Value> {
    let root = workspace_root()?;
    let path = root.join("lumenator/src/app/savings-text.cases.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} cannot be read: {e}", path.display()));
    Some(serde_json::from_str(&text).expect("the case list is JSON"))
}

fn billing(v: &Value) -> BillingView {
    match v.as_str().expect("billingMode") {
        "subscription" => BillingView::Subscription,
        "api" => BillingView::Api,
        "unset" => BillingView::Unset,
        "unreadable" => BillingView::Unreadable,
        other => panic!("billingMode {other:?}"),
    }
}

fn status(v: &Value) -> Status {
    match v["state"].as_str().expect("status.state") {
        "working" => Status::Working,
        "notWorking" => Status::NotWorking {
            reason: v["reason"].as_str().expect("reason").into(),
        },
        "noData" => Status::NoData,
        other => panic!("status {other:?}"),
    }
}

fn quality(v: &Value) -> DataQuality {
    match v["state"].as_str().expect("dataQuality.state") {
        "complete" => DataQuality::Complete,
        "partial" => DataQuality::Partial {
            reason: v["reason"].as_str().expect("reason").into(),
        },
        "empty" => DataQuality::Empty,
        other => panic!("dataQuality {other:?}"),
    }
}

/// A token band as the case names it: a number (no width) or `{low, value, high}`.
fn token_band(v: &Value) -> Band<i64> {
    match v {
        Value::Object(_) => Band {
            low: v["low"].as_i64().expect("low"),
            value: v["value"].as_i64().expect("value"),
            high: v["high"].as_i64().expect("high"),
        },
        _ => Band::exact(v.as_i64().unwrap_or(0)),
    }
}

/// A period as the case names it, on top of an empty one. Gross and extra rounds default to the
/// net and nothing, as in the app's builder.
fn period(mut p: Period, v: &Value) -> Period {
    p.tokens_saved = token_band(&v["tokensSaved"]);
    p.share_pct = v["sharePct"].as_f64();
    p.measured = v["net"].as_f64().map(|net| Dollars {
        rows: 1,
        gross: Band::exact(v["gross"].as_f64().unwrap_or(net)),
        extra_rounds: Band::exact(v["extraRounds"].as_f64().unwrap_or(0.0)),
        net: Band::exact(net),
        high_open: false,
    });
    p.modelled = v["modelled"].as_f64().map(|m| Modelled {
        rows: 1,
        value: Band::exact(m),
    });
    p.unpriced.calls = v["unpricedCalls"].as_i64().unwrap_or(0);
    p.counts.calls = v["calls"].as_i64().unwrap_or(0);
    p.counts.blocked_reads = v["blockedReads"].as_i64().unwrap_or(0);
    p.counts.blocked_reads_from_transcripts =
        v["blockedReadsFromTranscripts"].as_i64().unwrap_or(0);
    p.counts.blocked_reads_from_faults = v["blockedReadsFromFaults"].as_i64().unwrap_or(0);
    p.counts.blocked_reads_bounded = v["blockedReadsBounded"].as_i64().unwrap_or(0);
    p.data_quality = quality(&v["dataQuality"]);
    p
}

/// The case's summary, on top of an empty ledger's.
fn summary(v: &Value) -> SavingsSummary {
    let mut s = summarize(
        &Ledger::default(),
        &Clock {
            now_ms: 1_791_979_200_000,
            week_start_ms: 1_791_763_200_000,
            week_start: "2026-10-12".into(),
        },
        &Facts {
            billing_mode: billing(&v["billingMode"]),
            identity_hook_registered: None,
        },
        &Calibrated,
    );
    s.status = status(&v["status"]);
    s.recommendation = None;
    if v["recommendation"].is_object() {
        // The text is the engine's own; the code must be one the engine sends.
        let code: &'static str = match v["recommendation"]["code"].as_str().unwrap() {
            "smallSavings" => "smallSavings",
            "blocksWithoutSavings" => "blocksWithoutSavings",
            other => panic!("recommendation {other:?}"),
        };
        s.recommendation = Some(Recommendation {
            code,
            text: v["recommendation"]["text"].as_str().unwrap().into(),
        });
    }
    s.week = period(s.week.clone(), &v["week"]);
    s.all_time = period(s.all_time.clone(), &v["allTime"]);
    s
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// An empty piece of a phrase is no piece, as the case list writes it.
fn piece(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

#[test]
fn every_figure_formats_as_the_app_formats_it() {
    let Some(doc) = cases() else { return };
    for c in doc["usd"].as_array().unwrap() {
        let x = c["x"].as_f64().unwrap();
        assert_eq!(
            text::usd_signed(x),
            c["signed"].as_str().unwrap(),
            "usd_signed({x})"
        );
        assert_eq!(
            text::usd_plus(x),
            c["plus"].as_str().unwrap(),
            "usd_plus({x})"
        );
        assert_eq!(
            text::usd_phrase(x),
            c["phrase"].as_str().unwrap(),
            "usd_phrase({x})"
        );
    }
    for c in doc["tokens"].as_array().unwrap() {
        let n = c["n"].as_i64().unwrap();
        assert_eq!(
            text::tokens_signed(n),
            c["signed"].as_str().unwrap(),
            "tokens_signed({n})"
        );
        assert_eq!(
            text::tokens_figure(n),
            c["figure"].as_str().unwrap(),
            "tokens_figure({n})"
        );
        assert_eq!(
            text::tokens_phrase(n),
            c["phrase"].as_str().unwrap(),
            "tokens_phrase({n})"
        );
    }
    for c in doc["tokensBand"].as_array().unwrap() {
        let b = token_band(c);
        assert_eq!(
            text::tokens_band(&b),
            c["text"].as_str().unwrap(),
            "tokens_band({b:?})"
        );
    }
    for c in doc["share"].as_array().unwrap() {
        let pct = c["pct"].as_f64().unwrap();
        assert_eq!(
            text::share_text(pct),
            c["text"].as_str().unwrap(),
            "share_text({pct})"
        );
    }
}

#[test]
fn every_state_reads_the_same_in_the_terminal_the_tray_and_the_app() {
    let Some(doc) = cases() else { return };
    let states = doc["states"].as_array().unwrap();
    assert!(states.len() >= 12, "too few states to mean anything");
    for c in states {
        let name = c["name"].as_str().unwrap();
        let s = summary(&c["summary"]);
        assert_eq!(
            text::headline_line(&s),
            c["cli"].as_str().unwrap(),
            "{name}: lumen"
        );
        assert_eq!(text::tray_line(&s), opt(&c["tray"]), "{name}: the tray");
        // The band behind each central token figure, where the case gives one: the app's ⓘ and
        // `lumen report` say it in these words.
        if let Some(bands) = c.get("bands") {
            for (key, p) in [("week", &s.week), ("allTime", &s.all_time)] {
                assert_eq!(
                    Some(text::tokens_band(&p.tokens_saved)),
                    opt(&bands[key]),
                    "{name}.{key}: band"
                );
                assert!(
                    p.tokens_saved.low < p.tokens_saved.high,
                    "{name}.{key}: a band"
                );
            }
        }

        // The pieces the app's headline is made of, from the same functions as the line.
        for (key, p, when) in [
            ("week", &s.week, "this week"),
            ("allTime", &s.all_time, "all time"),
        ] {
            let app = &c["app"][key];
            let empty = p.data_quality == DataQuality::Empty;
            assert_eq!(
                empty,
                app["empty"].as_bool().unwrap(),
                "{name}.{key}: empty"
            );
            if empty {
                // Said in words, never as a zero.
                let line = text::headline_line(&s);
                let said = if s.all_time.data_quality == DataQuality::Empty {
                    "no Lumen activity recorded yet".to_string()
                } else {
                    format!("{when}: no Lumen activity")
                };
                assert!(line.contains(&said), "{name}.{key}: {line}");
                continue;
            }
            let share = if text::shows_share(s.billing_mode) {
                text::share_line(p, when)
            } else {
                None
            };
            assert_eq!(share, opt(&app["share"]), "{name}.{key}: share");
            let n = p.tokens_saved.value;
            assert_eq!(
                Some(text::tokens_figure(n)),
                opt(&app["tokens"]),
                "{name}.{key}: tokens"
            );
            assert_eq!(
                Some(text::tokens_unit(n).to_string()),
                opt(&app["tokensUnit"]),
                "{name}.{key}: tokens unit"
            );
            // The bold dollar figure is the measured net, and nothing else.
            let parts = p.measured.map(|d| text::usd_parts(d.net.value));
            assert_eq!(
                parts.as_ref().and_then(|(lead, _, _)| piece(lead)),
                opt(&app["usdLead"]),
                "{name}.{key}: dollars lead"
            );
            assert_eq!(
                parts.as_ref().map(|(_, figure, _)| figure.clone()),
                opt(&app["usd"]),
                "{name}.{key}: dollars"
            );
            assert_eq!(
                parts.as_ref().and_then(|(_, _, tail)| piece(tail)),
                opt(&app["usdTail"]),
                "{name}.{key}: dollars tail"
            );
            if let Some(d) = p.measured
                && d.gross.value != d.net.value
            {
                let gross = text::usd_parts(d.gross.value).1;
                assert_ne!(opt(&app["usd"]), Some(gross), "{name}.{key}: the gross");
            }
            assert_eq!(
                text::price_basis(s.billing_mode).map(str::to_string),
                opt(&app["basis"]),
                "{name}.{key}: basis"
            );
            let beside: Vec<String> = app["beside"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_str().unwrap().to_string())
                .collect();
            assert_eq!(text::beside_parts(p), beside, "{name}.{key}: beside");
            let partial = match &p.data_quality {
                DataQuality::Partial { reason } => Some(reason.clone()),
                _ => None,
            };
            assert_eq!(partial, opt(&app["partial"]), "{name}.{key}: partial");
        }
    }

    let root = workspace_root().unwrap();
    for spec in [
        "lumenator/src/app/savings-text.spec.ts",
        "lumenator/src/app/components/savings-headline/savings-headline.spec.ts",
    ] {
        let t = std::fs::read_to_string(root.join(spec)).expect(spec);
        assert!(
            t.contains("savings-text.cases.json'"),
            "{spec} no longer reads the shared case list"
        );
    }
}

#[test]
fn the_window_reads_the_schema_the_engine_writes() {
    // The window refuses a summary of another schema rather than reading new meanings into old
    // names; that holds only while the number it expects is the engine's.
    let Some(root) = workspace_root() else { return };
    let path = root.join("lumenator/src/app/savings-summary.ts");
    let ts = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} cannot be read: {e}", path.display()));
    let want = format!(
        "export const SAVINGS_SCHEMA = {};",
        lumen_core::savings::SCHEMA
    );
    assert!(ts.contains(&want), "savings-summary.ts does not say {want}");
}
