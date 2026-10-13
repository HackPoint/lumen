//! The summary in a line: what bare `lumen` prints and what the tray menu says.
//!
//! Both are made here from the same [`SavingsSummary`], so the terminal and the menu bar cannot
//! say two different things. Signed: a loss reads as one. The dollar figure is the measured one
//! ([`super::Period::measured`]), a floor, so it reads "at least"; the modelled dollars and the
//! calls with no dollars are said beside it, never added to it. Tokens read `≈`: they are the
//! central value of a band.

use super::{BillingView, DataQuality, Period, SavingsSummary, Status};

/// `$1.23`, `−$0.40` (a true minus sign), whole dollars from $100. The sign is the value's, never
/// the rounding's: a loss of less than half a cent reads `−$0.00`, never `$0.00`, and only a value
/// of exactly zero reads `$0.00`.
pub fn usd_signed(x: f64) -> String {
    let abs = x.abs();
    let digits = if abs >= 100.0 {
        format!("{abs:.0}")
    } else {
        format!("{abs:.2}")
    };
    if x < 0.0 {
        format!("\u{2212}${digits}")
    } else {
        format!("${digits}")
    }
}

/// `12,345`, `−12,345`.
pub fn tokens_signed(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    if n < 0 {
        format!("\u{2212}{grouped}")
    } else {
        grouped
    }
}

fn status_word(s: &Status) -> &'static str {
    match s {
        Status::Working => "working",
        Status::NotWorking { .. } => "not working",
        Status::NoData => "no data",
    }
}

/// `+$1.23`, `−$0.40`: an amount beside the bold figure, its sign always shown, by the same rule
/// as [`usd_signed`] (`+$0.00` is a gain of less than half a cent; `$0.00` is zero).
pub fn usd_plus(x: f64) -> String {
    if x > 0.0 {
        format!("+{}", usd_signed(x))
    } else {
        usd_signed(x)
    }
}

/// What a period's bold figure leaves out, briefly, with a leading space: where `dollars` are
/// shown, the modelled dollars and the calls with tokens and no dollars; and, where blocked Reads
/// are all the period holds, how many, since the figure is then theirs alone. Empty when there is
/// nothing to say.
fn beside(p: &Period, dollars: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if p.counts.calls == 0 {
        match p.counts.blocked_reads {
            0 => {}
            1 => parts.push("1 blocked Read".to_string()),
            n => parts.push(format!("{} blocked Reads", tokens_signed(n))),
        }
    }
    if dollars {
        if let Some(m) = p.modelled {
            parts.push(format!("{} modelled", usd_plus(m.value.value)));
        }
        match p.unpriced.calls {
            0 => {}
            1 => parts.push("1 call unpriced".to_string()),
            n => parts.push(format!("{} calls unpriced", tokens_signed(n))),
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join("; "))
    }
}

/// A period's dollars where a surface shows them, for `when` ("this week", "all time"): the
/// measured net, "at least", and what it leaves out beside it. Modelled dollars are never the
/// figure itself. `None` when there is nothing to say.
fn period_usd(p: &Period, when: &str) -> Option<String> {
    let beside = beside(p, true);
    match p.measured {
        Some(d) => Some(format!(
            "at least {} net {when}{beside}",
            usd_signed(d.net.value)
        )),
        None if !beside.is_empty() => Some(format!("no dollars measured {when}{beside}")),
        None => None,
    }
}

/// The week as the tray says it: dollars for an API or not-yet-asked user when the week measured
/// any, otherwise tokens, and what the figure leaves out beside it. A subscription is not billed
/// per token, so a dollar figure would be money it did not save.
fn week_figure(s: &SavingsSummary) -> String {
    let tokens = format!(
        "\u{2248} {} tokens this week",
        tokens_signed(s.week.tokens_saved.value)
    );
    if s.billing_mode == BillingView::Subscription {
        return format!("{tokens}{}", beside(&s.week, false));
    }
    let beside = beside(&s.week, true);
    match s.week.measured {
        Some(d) => format!("at least {} this week{beside}", usd_signed(d.net.value)),
        None => format!("{tokens}{beside}"),
    }
}

/// The tray menu's savings line, or `None` when there is nothing to say: no line at all rather
/// than an empty or a placeholder one.
pub fn tray_line(s: &SavingsSummary) -> Option<String> {
    if s.status == Status::NoData {
        return None;
    }
    let state = status_word(&s.status);
    if s.week.data_quality == DataQuality::Empty {
        return Some(format!("No Lumen calls this week \u{b7} {state}"));
    }
    Some(format!("Saved {} \u{b7} {state}", week_figure(s)))
}

/// The one line bare `lumen` prints.
pub fn headline_line(s: &SavingsSummary) -> String {
    const MORE: &str = "lumen report for detail, lumen dash for the live dashboard";
    if s.all_time.data_quality == DataQuality::Empty {
        return format!(
            "Lumen: no Lumen call recorded yet \u{b7} {} ({MORE})",
            status_word(&s.status)
        );
    }
    // A subscription sees no dollars, and what stands beside its tokens is the blocked Reads.
    let tokens = |p: &Period, when: &str| match s.billing_mode {
        BillingView::Subscription => format!(
            "{} tokens {when}{}",
            tokens_signed(p.tokens_saved.value),
            beside(p, false)
        ),
        _ => format!("{} tokens {when}", tokens_signed(p.tokens_saved.value)),
    };
    let usd: String = match s.billing_mode {
        BillingView::Subscription => String::new(),
        _ => [
            period_usd(&s.week, "this week"),
            period_usd(&s.all_time, "all time"),
        ]
        .into_iter()
        .flatten()
        .map(|p| format!(", {p}"))
        .collect(),
    };
    format!(
        "Lumen: saved \u{2248} {}, {}{usd} \u{b7} {} ({MORE})",
        tokens(&s.week, "this week"),
        tokens(&s.all_time, "all time"),
        status_word(&s.status)
    )
}
