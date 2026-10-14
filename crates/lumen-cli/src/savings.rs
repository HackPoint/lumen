//! `lumen` and `lumen report`: the savings summary, in a line and in detail.
//!
//! The figures are `lumen_core::savings`'s, read through its own rusqlite loader. Nothing here
//! computes one; it only lays the summary out for a terminal.

use lumen_core::savings::{
    self, Band, BillingView, CallLine, DataQuality, Dollars, Facts, LineKind, Period,
    SavingsSummary, Status, Uncalibrated, text,
};

/// The summary of this machine's ledger at `as_of_ms`.
pub fn summary(as_of_ms: i64) -> Result<SavingsSummary, String> {
    let db = lumen_core::meter::db_path()
        .ok_or_else(|| "cannot resolve a database path; set LUMEN_DB".to_string())?;
    // connect_db brings an older ledger's schema up to date, as the app's opener does, so the
    // two read the same columns. No ledger yet is a machine that has recorded nothing: it is read
    // as an empty one in memory, which reports exactly that, rather than created on disk by a
    // command that only reads.
    let conn = if db.exists() {
        lumen_core::meter::connect_db(&db)
    } else {
        lumen_core::meter::connect_db(std::path::Path::new(":memory:"))
    }
    .map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let facts = Facts {
        billing_mode: BillingView::of(&lumen_core::settings::billing_mode_for(&db)),
        identity_hook_registered: identity_hook_registered(),
    };
    let spool = lumen_core::faults::spool_path();
    savings::load::summary(&conn, as_of_ms, spool.as_deref(), &facts, &Uncalibrated)
}

/// Whether Claude Code's settings register Lumen's identity hook: the one predicate the app's
/// prompt and `lumen doctor` call, on the file read the way Setup reads it.
fn identity_hook_registered() -> Option<bool> {
    let path = dirs::home_dir()?.join(".claude").join("settings.json");
    lumen_core::identity_hook::registered_in(&path)
}

fn usd_band(b: &Band<f64>) -> String {
    format!(
        "{} (band {} to {})",
        text::usd_signed(b.value),
        text::usd_signed(b.low),
        text::usd_signed(b.high)
    )
}

/// One part of a period's dollars: its rows, value, extra rounds and net.
fn dollars(out: &mut String, d: &Dollars, high_open_why: &str) {
    out.push_str(&format!(
        "    value of the tokens   {}\n",
        usd_band(&d.gross)
    ));
    out.push_str(&format!(
        "    extra rounds          {}\n",
        usd_band(&d.extra_rounds)
    ));
    out.push_str(&format!(
        "    net                   {}{}\n",
        usd_band(&d.net),
        if d.high_open {
            format!(", the band's high end is not a bound ({high_open_why})")
        } else {
            String::new()
        }
    ));
}

fn tokens_band(b: &Band<i64>) -> String {
    if b.low == b.high {
        text::tokens_signed(b.value)
    } else {
        format!(
            "{} (band {} to {})",
            text::tokens_signed(b.value),
            text::tokens_signed(b.low),
            text::tokens_signed(b.high)
        )
    }
}

fn period(out: &mut String, title: &str, when: &str, p: &Period, billing: BillingView) {
    out.push_str(&format!("{title}\n"));
    if p.data_quality == DataQuality::Empty {
        out.push_str("  no Lumen activity recorded\n\n");
        return;
    }
    // The first figure on Home where dollars are at API prices, said here in the same words.
    if text::shows_share(billing)
        && let Some(share) = text::share_line(p, when)
    {
        out.push_str(&format!("  {share}\n"));
    }
    let basis = match text::price_basis(billing) {
        Some(b) => format!(" {b}"),
        None => String::new(),
    };
    let c = &p.counts;
    out.push_str(&format!(
        "  tokens saved, net     {}\n",
        tokens_band(&p.tokens_saved)
    ));
    if let Some(share) = p.share_pct {
        out.push_str(&format!(
            "  share of the files    {share:.1}% of {} tokens they held\n",
            text::tokens_signed(p.tokens_full.value)
        ));
    }
    let rows = |n: i64, what: &str| {
        if n == 1 {
            format!("1 {what}")
        } else {
            format!("{} {what}s", text::tokens_signed(n))
        }
    };
    match &p.measured {
        Some(d) => {
            out.push_str(&format!(
                "  measured               {}{basis}, from {}\n",
                text::usd_phrase(d.net.value),
                rows(d.rows, "row")
            ));
            dollars(
                out,
                d,
                "a session may still be running, or R may be too long and was taken at 0",
            );
        }
        None => out.push_str("  measured               no dollars measured\n"),
    }
    for why in &p.usd_shortfalls {
        out.push_str(&format!("    {why}\n"));
    }
    if let Some(m) = &p.modelled {
        out.push_str(&format!(
            "  modelled, not added    {}, cache reads over the R of {} that may be too long \
             (band {} to {})\n",
            text::usd_plus(m.value.value),
            rows(m.rows, "call"),
            text::usd_plus(m.value.low),
            text::usd_plus(m.value.high)
        ));
    }
    let u = &p.unpriced;
    if u.calls + u.blocked_reads > 0 {
        let mut why: Vec<String> = Vec::new();
        if u.no_session > 0 {
            why.push(format!(
                "{} no session recorded",
                text::tokens_signed(u.no_session)
            ));
        }
        if u.no_turn > 0 {
            why.push(format!(
                "{} no turn that read it",
                text::tokens_signed(u.no_turn)
            ));
        }
        if u.no_price > 0 {
            why.push(format!(
                "{} a model with no price",
                text::tokens_signed(u.no_price)
            ));
        }
        out.push_str(&format!(
            "  tokens only            {}{}, {} tokens: {}\n",
            rows(u.calls, "call"),
            if u.blocked_reads > 0 {
                format!(" and {}", rows(u.blocked_reads, "blocked Read"))
            } else {
                String::new()
            },
            text::tokens_signed(u.tokens.value),
            why.join(", ")
        ));
    }
    out.push_str(&format!(
        "  {} calls, {} blocked Reads ({} recorded in transcripts, {} from escape-valve faults{}; \
         {} escape-valve faults); {} with a derived R and C, {} with an unverified count\n",
        c.calls,
        c.blocked_reads,
        c.blocked_reads_from_transcripts,
        c.blocked_reads_from_faults,
        if c.blocked_reads_bounded > 0 {
            format!(
                ", {} bounded, one per agent that could have read the path, none known",
                c.blocked_reads_bounded
            )
        } else {
            String::new()
        },
        c.valve_faults,
        c.derived_pairs,
        c.unverified_provenance_rows
    ));
    if c.rounds_collapse_rows > 0 || c.rounds_unrecorded_rows > 0 {
        out.push_str(&format!(
            "  R: {} ended by a collapse (inferred), {} from before compactions were recorded \
             (may be too long)\n",
            c.rounds_collapse_rows, c.rounds_unrecorded_rows
        ));
    }
    out.push_str(&format!(
        "  extra rounds: {} on evidence, {} modelled{}\n",
        c.extra_rounds_evidenced,
        c.extra_rounds_modelled,
        if c.rounds_lower_bound_rows > 0 {
            format!(
                "; {} R from a session that may still be running (a lower bound)",
                c.rounds_lower_bound_rows
            )
        } else {
            String::new()
        }
    ));
    match &p.data_quality {
        DataQuality::Complete => out.push_str("  data: complete\n"),
        DataQuality::Partial { reason } => out.push_str(&format!("  partial data: {reason}\n")),
        DataQuality::Empty => {}
    }
    out.push('\n');
}

fn line(l: &CallLine) -> String {
    let figure = match l.net_usd {
        Some(n) => text::usd_signed(n.value),
        None => format!("{} tokens", text::tokens_signed(l.tokens_saved.value)),
    };
    let modelled = match l.modelled_value_usd {
        Some(m) => format!("; {} modelled", text::usd_plus(m.value)),
        None => String::new(),
    };
    let what = match l.kind {
        LineKind::Call => format!(
            "{} {}",
            l.tool.unwrap_or("?"),
            l.path.as_deref().unwrap_or("")
        ),
        LineKind::BlockedRead => format!(
            "blocked Read{}: {}",
            match l.extra_round_basis {
                "blockedReadRecorded" => ", from its transcript",
                _ => ", then let through",
            },
            l.path.as_deref().unwrap_or("")
        ),
    };
    let pair = match (&l.pair.model, l.pair.rounds, l.pair.rounds_bound) {
        (Some(m), Some(r), Some(b)) => format!(
            "{m}, R {r} {}",
            match b {
                "compaction" => "to a compaction",
                "rewind" => "to a rewind",
                "collapse" => "to a collapse in the prompt (inferred)",
                "sessionOpen" => "so far, the session may still be running",
                "sessionEndUnrecorded" => "to the session's end, unrecorded (may be too long)",
                _ => "to the session's end",
            }
        ),
        (Some(m), _, _) => m.clone(),
        _ => format!("no dollars: {}", l.pair.basis),
    };
    format!(
        "  {figure:>12}  {}  {what}  ({} tokens saved; {pair}; extra round: {}{modelled})\n",
        l.ts,
        text::tokens_signed(l.tokens_saved.value),
        l.extra_round_basis
    )
}

/// `lumen report`: everything the summary holds, for a person.
pub fn render_detail(s: &SavingsSummary) -> String {
    let mut out = String::new();
    let billing = match s.billing_mode {
        BillingView::Subscription => {
            "subscription (dollars at API prices; a plan is not billed per token)"
        }
        BillingView::Api => "API",
        BillingView::Unset => "not set (dollars at API prices)",
        BillingView::Unreadable => "unreadable setting (dollars at API prices)",
    };
    out.push_str(&format!(
        "Lumen savings as of {}. Billing: {billing}.\n\n",
        s.as_of
    ));
    period(
        &mut out,
        &format!("This week, {}", s.method.week),
        "this week",
        &s.week,
        s.billing_mode,
    );
    period(
        &mut out,
        &match &s.all_time.from {
            Some(f) => format!("All time, since {f}"),
            None => "All time".to_string(),
        },
        "all time",
        &s.all_time,
        s.billing_mode,
    );
    match &s.status {
        Status::Working => out.push_str("Status: working\n"),
        Status::NotWorking { reason } => out.push_str(&format!("Status: not working: {reason}\n")),
        Status::NoData => out.push_str("Status: no data\n"),
    }
    if let Some(r) = &s.recommendation {
        out.push_str(&format!("Recommendation: {}\n", r.text));
    }
    out.push('\n');

    let m = &s.method;
    out.push_str("How it is measured\n");
    out.push_str(&format!(
        "  prices: list prices per model, speed and cache duration, as of {}\n",
        m.prices_as_of
    ));
    out.push_str(&format!(
        "  calibration: {}{}\n",
        if m.calibration.calibrated {
            "calibrated"
        } else {
            "uncalibrated"
        },
        match &m.calibration.as_of {
            Some(d) => format!(" as of {d}. {}", m.calibration.note),
            None => format!(". {}", m.calibration.note),
        }
    ));
    out.push_str(&format!("  saved against: {}\n", m.counterfactual));
    out.push_str(&format!("  R: {}\n", m.rounds));
    out.push_str(&format!("  C: {}\n", m.round_cost));
    out.push_str(&format!("  extra rounds: {}\n", m.extra_rounds));
    out.push_str(&format!("  unrecorded prices: {}\n", m.floors));
    out.push_str(&format!("  dollars: {}\n", m.parts));
    out.push('\n');

    if !s.ledger.top_gains.is_empty() {
        out.push_str("Top contributing\n");
        for l in &s.ledger.top_gains {
            out.push_str(&line(l));
        }
        out.push('\n');
    }
    if !s.ledger.top_losses.is_empty() {
        out.push_str("Top losing\n");
        for l in &s.ledger.top_losses {
            out.push_str(&line(l));
        }
        out.push('\n');
    }
    out.push_str("Per call: lumen report --json\n");
    out
}

/// What bare `lumen` prints when the summary cannot be made.
pub fn headline_error(e: &str) -> String {
    format!(
        "Lumen: no savings figure ({e}) (lumen report for detail, lumen dash for the live \
         dashboard)"
    )
}

/// `--as-of`: unix milliseconds, or an ISO-8601 time.
pub fn parse_as_of(v: &str) -> Result<i64, String> {
    v.trim()
        .parse::<i64>()
        .ok()
        .or_else(|| savings::parse_ts_ms(v))
        .ok_or_else(|| format!("{v:?} is neither unix milliseconds nor an ISO-8601 time"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::savings::{Clock, Ledger, Modelled, summarize};

    fn empty() -> SavingsSummary {
        summarize(
            &Ledger::default(),
            &Clock {
                now_ms: 1_791_806_400_000,
                week_start_ms: 1_791_763_200_000,
                week_start: "2026-10-12".into(),
            },
            &Facts {
                billing_mode: BillingView::Unset,
                identity_hook_registered: None,
            },
            &Uncalibrated,
        )
    }

    #[test]
    fn the_detail_of_an_empty_ledger_says_nothing_was_recorded_and_how_it_would_be_measured() {
        let d = render_detail(&empty());
        assert!(d.contains("no Lumen activity recorded"), "{d}");
        assert!(d.contains("Status: no data"), "{d}");
        assert!(d.contains("uncalibrated"), "{d}");
        assert!(d.contains("2026-10-13"), "the price table's date: {d}");
        assert!(
            !d.contains('$'),
            "no dollar figure where nothing was priced: {d}"
        );
    }

    #[test]
    fn a_period_shows_its_measured_modelled_and_tokens_only_parts_apart() {
        let mut s = empty();
        s.week.data_quality = DataQuality::Partial {
            reason: "2 rows with no session recorded, so tokens and no dollars".into(),
        };
        let band = |v: f64| Band {
            low: v - 0.05,
            value: v,
            high: v + 0.05,
        };
        s.week.measured = Some(Dollars {
            rows: 4,
            gross: band(0.5),
            extra_rounds: band(0.55),
            net: band(-0.05),
            high_open: false,
        });
        s.week.modelled = Some(Modelled {
            rows: 1,
            value: band(1.8),
        });
        s.week.unpriced.calls = 2;
        s.week.unpriced.no_session = 2;
        s.week.unpriced.tokens = Band::exact(9_000);
        s.week.counts.calls = 7;
        let d = render_detail(&s);
        assert!(
            d.contains(
                "measured               net loss of at most $0.05 at API prices, from 4 rows"
            ),
            "{d}"
        );
        assert!(
            d.contains(
                "modelled, not added    +$1.80, cache reads over the R of 1 call that may be too \
                 long"
            ),
            "{d}"
        );
        assert!(
            d.contains("tokens only            2 calls, 9,000 tokens: 2 no session recorded"),
            "{d}"
        );
    }

    #[test]
    fn as_of_takes_milliseconds_or_an_iso_time() {
        assert_eq!(parse_as_of("1791806400000"), Ok(1_791_806_400_000));
        assert_eq!(parse_as_of("2026-10-12T12:00:00Z"), Ok(1_791_806_400_000));
        assert!(parse_as_of("tuesday").is_err());
    }
}
