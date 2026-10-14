//! The summary in words: what bare `lumen` prints, what the tray menu says, and the phrases the
//! app's headline is made of.
//!
//! The terminal and the menu bar are made here from the same [`SavingsSummary`], so they cannot
//! say two different things. The app renders the same summary in TypeScript
//! (`lumenator/src/app/savings-text.ts`), phrase for phrase: `lumenator/src/app/savings-text.cases.json`
//! holds the numbers and the summaries both are tested against
//! (`crates/lumen-core/tests/frontend_savings_text.rs` here, `savings-text.spec.ts` there), so
//! the window cannot say another figure, sign or label either.
//!
//! The dollar figure is the measured one ([`super::Period::measured`]), a floor: a gain reads "at
//! least $X saved, net", and a loss, whose floor is the most it can be, "net loss of at most $X"
//! (or "net loss under half a cent"); the modelled dollars and the calls with no dollars are said
//! beside it, never added to it. Every billing mode gets the dollars: on the API they are what
//! the user paid less, and otherwise they are said "at API prices", since a plan is not billed per
//! token, after the share of tokens Lumen kept out of context. Tokens read `≈`: they are the
//! central value of a band. A figure that is not a number is never printed as one.

use super::{BillingView, DataQuality, Period, SavingsSummary, Status};

/// What the dollars are said to be at where the user does not pay per token: a subscription,
/// or a billing mode not chosen or not readable.
pub const AT_API_PRICES: &str = "at API prices";

/// What a dollar or share figure that is not a finite number reads as: never `NaN` or `inf`.
pub const NO_FIGURE: &str = "no figure";

/// `≈` and a no-break space: the sign and its number never wrap apart.
const ABOUT: &str = "\u{2248}\u{a0}";

/// `1234567` as `1,234,567`.
fn grouped(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The digits of a dollar amount: cents below $100, whole dollars from it, grouped by thousands.
/// Rounded as Rust rounds, half to even on the exact value (`0.125` is `0.12`), which the app's
/// copy reproduces.
fn usd_digits(abs: f64) -> String {
    if abs >= 100.0 {
        grouped(&format!("{abs:.0}"))
    } else {
        format!("{abs:.2}")
    }
}

/// `$1.23`, `−$0.40` (a true minus sign), `$12,346`. The sign is the value's, never the
/// rounding's: a loss of less than half a cent reads `−$0.00`, never `$0.00`, and only a value of
/// exactly zero reads `$0.00`. Not a number: [`NO_FIGURE`].
pub fn usd_signed(x: f64) -> String {
    if !x.is_finite() {
        return NO_FIGURE.to_string();
    }
    let digits = usd_digits(x.abs());
    if x < 0.0 {
        format!("\u{2212}${digits}")
    } else {
        format!("${digits}")
    }
}

/// `+$1.23`, `−$0.40`: an amount beside the bold figure, its sign always shown, by the same rule
/// as [`usd_signed`] (`+$0.00` is a gain of less than half a cent; `$0.00` is zero).
pub fn usd_plus(x: f64) -> String {
    if x > 0.0 && x.is_finite() {
        format!("+{}", usd_signed(x))
    } else {
        usd_signed(x)
    }
}

/// `12,345`, `−12,345`.
pub fn tokens_signed(n: i64) -> String {
    let digits = grouped(&n.unsigned_abs().to_string());
    if n < 0 {
        format!("\u{2212}{digits}")
    } else {
        digits
    }
}

/// `≈ 12,345`: a token figure, the central value of its band, as its size; whether it was saved
/// or added is the unit's to say ([`tokens_unit`]).
pub fn tokens_figure(n: i64) -> String {
    format!("{ABOUT}{}", grouped(&n.unsigned_abs().to_string()))
}

/// What a token figure is: `tokens saved, net`, or `more tokens, net` for a period that sent more
/// than it kept out.
pub fn tokens_unit(n: i64) -> &'static str {
    if n < 0 {
        "more tokens, net"
    } else {
        "tokens saved, net"
    }
}

/// `≈ 12,345 tokens saved, net`, `≈ 32 more tokens, net`.
pub fn tokens_phrase(n: i64) -> String {
    format!("{} {}", tokens_figure(n), tokens_unit(n))
}

/// The measured net in three pieces, the figure the bold one: `at least` `$0.20` `saved, net`;
/// for a loss, whose floor is the most it can be, `net loss of at most` `$0.42` and nothing after;
/// for a loss that rounds to no cent, `net loss` `under half a cent`. Not a number: no figure.
pub fn usd_parts(x: f64) -> (&'static str, String, &'static str) {
    if !x.is_finite() {
        return ("", NO_FIGURE.to_string(), "");
    }
    if x >= 0.0 {
        return ("at least", usd_signed(x), "saved, net");
    }
    let digits = usd_digits(x.abs());
    if digits == "0.00" {
        ("net loss", "under half a cent".to_string(), "")
    } else {
        ("net loss of at most", format!("${digits}"), "")
    }
}

/// [`usd_parts`] as one phrase: `at least $0.20 saved, net`, `net loss of at most $0.42`.
pub fn usd_phrase(x: f64) -> String {
    let (lead, figure, tail) = usd_parts(x);
    [lead, figure.as_str(), tail]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The change in tokens a share of them kept out of context makes: `92% fewer` for 92% kept out,
/// `5.0% more` for 5% more sent. Whole percents from 10, one decimal below; `0.0% fewer` is a
/// saving under a twentieth of a percent, and only none at all is `0% fewer`.
pub fn share_text(pct: f64) -> String {
    if !pct.is_finite() {
        return NO_FIGURE.to_string();
    }
    let abs = pct.abs();
    let digits = if abs >= 10.0 {
        format!("{abs:.0}")
    } else {
        format!("{abs:.1}")
    };
    if pct > 0.0 {
        format!("{digits}% fewer")
    } else if pct < 0.0 {
        format!("{digits}% more")
    } else {
        "0% fewer".to_string()
    }
}

/// `≈ 92% fewer tokens from file and tool reads`; `None` with nothing read.
pub fn share_phrase(p: &Period) -> Option<String> {
    p.share_pct
        .map(|s| format!("{ABOUT}{} tokens from file and tool reads", share_text(s)))
}

/// [`share_phrase`] for `when`: `≈ 92% fewer tokens from file and tool reads this week`.
pub fn share_line(p: &Period, when: &str) -> Option<String> {
    share_phrase(p).map(|s| format!("{s} {when}"))
}

/// What the dollars are at, said beside them: nothing on the API, where they are the user's own
/// price, and [`AT_API_PRICES`] otherwise.
pub fn price_basis(b: BillingView) -> Option<&'static str> {
    match b {
        BillingView::Api => None,
        BillingView::Subscription | BillingView::Unset | BillingView::Unreadable => {
            Some(AT_API_PRICES)
        }
    }
}

/// Whether the share of tokens is said first: wherever the dollars are at API prices, since
/// tokens are then the figure the user is billed by, or may be.
pub fn shows_share(b: BillingView) -> bool {
    price_basis(b).is_some()
}

fn status_word(s: &Status) -> &'static str {
    match s {
        Status::Working => "working",
        Status::NotWorking { .. } => "not working",
        Status::NoData => "no data",
    }
}

/// What a period's bold figures leave out, each part on its own: where blocked Reads are all the
/// period holds, how many, since the figure is then theirs alone; the modelled dollars; and the
/// calls with tokens and no dollars. The app shows each as a tag beside the figure; the terminal
/// and the menu say them in brackets ([`beside`]).
pub fn beside_parts(p: &Period) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if p.counts.calls == 0 {
        match p.counts.blocked_reads {
            0 => {}
            1 => parts.push("1 blocked Read".to_string()),
            n => parts.push(format!("{} blocked Reads", tokens_signed(n))),
        }
    }
    if let Some(m) = p.modelled {
        parts.push(format!("{} modelled", usd_plus(m.value.value)));
    }
    match p.unpriced.calls {
        0 => {}
        1 => parts.push("1 call unpriced".to_string()),
        n => parts.push(format!("{} calls unpriced", tokens_signed(n))),
    }
    parts
}

/// [`beside_parts`] with a leading space, in brackets, or empty when there is nothing to say.
fn beside(p: &Period) -> String {
    let parts = beside_parts(p);
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join("; "))
    }
}

/// The measured dollars as a phrase, at API prices where the user does not pay per token, or
/// that none were measured.
fn usd_clause(p: &Period, billing: BillingView) -> String {
    match (p.measured, price_basis(billing)) {
        (Some(d), Some(basis)) => format!("{} {basis}", usd_phrase(d.net.value)),
        (Some(d), None) => usd_phrase(d.net.value),
        (None, _) => "no dollars measured".to_string(),
    }
}

/// One period as the terminal says it, in the phrases of the app's box for it: the share where
/// the dollars are at API prices, the tokens, the measured net, and what the figure leaves out.
/// A period that recorded nothing says so rather than showing a zero.
fn period_body(s: &SavingsSummary, p: &Period) -> String {
    if p.data_quality == DataQuality::Empty {
        return "no Lumen activity".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if shows_share(s.billing_mode)
        && let Some(share) = share_phrase(p)
    {
        parts.push(share);
    }
    parts.push(tokens_phrase(p.tokens_saved.value));
    parts.push(usd_clause(p, s.billing_mode));
    format!("{}{}", parts.join("; "), beside(p))
}

/// `net loss …` as the start of a line: `Net loss …`.
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The tray menu's savings line, or `None` when there is nothing to say: no line at all rather
/// than an empty or a placeholder one. On the API, the week's measured dollars alone; otherwise
/// the week as the terminal says it.
pub fn tray_line(s: &SavingsSummary) -> Option<String> {
    if s.status == Status::NoData {
        return None;
    }
    let state = status_word(&s.status);
    let w = &s.week;
    if w.data_quality == DataQuality::Empty {
        return Some(format!("No Lumen activity this week \u{b7} {state}"));
    }
    let figure = match (w.measured, s.billing_mode) {
        (Some(d), BillingView::Api) if d.net.value >= 0.0 => {
            format!("Saved at least {} this week", usd_signed(d.net.value))
        }
        (Some(d), BillingView::Api) => {
            format!("{} this week", capitalized(&usd_phrase(d.net.value)))
        }
        _ => {
            return Some(format!("This week: {} \u{b7} {state}", period_body(s, w)));
        }
    };
    Some(format!("{figure}{} \u{b7} {state}", beside(w)))
}

/// The one line bare `lumen` prints.
pub fn headline_line(s: &SavingsSummary) -> String {
    const MORE: &str = "lumen report for detail, lumen dash for the live dashboard";
    let state = status_word(&s.status);
    if s.all_time.data_quality == DataQuality::Empty {
        return format!("Lumen: no Lumen activity recorded yet \u{b7} {state} ({MORE})");
    }
    format!(
        "Lumen this week: {} \u{b7} all time: {} \u{b7} {state} ({MORE})",
        period_body(s, &s.week),
        period_body(s, &s.all_time)
    )
}
