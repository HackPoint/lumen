//! Turn chains: which turn read a call's result, how many rounds followed it, and what each cost.
//!
//! See the module docs of [`super`] for the rules; this is their arithmetic.

use std::collections::HashMap;

use crate::rates::{self, Prices, Tokens};

// ── time ─────────────────────────────────────────────────────────────────────

/// Days from 1970-01-01 to a civil date (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count (civil_from_days).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn digits(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Unix milliseconds of a timestamp as the ledger writes them: `2026-06-01T16:14:05Z` (the meter,
/// to the second), `2026-06-01T16:14:05.157Z` (a transcript), with an offset (`+02:00`) or none
/// (read as UTC), and with a space for the `T` (SQLite's own format). `None` for anything else:
/// a time that cannot be read places a row in no week rather than a guessed one.
pub fn parse_ts_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 || !s.is_char_boundary(10) || !s.is_char_boundary(19) {
        return None;
    }
    let (date, rest) = s.split_at(10);
    let sep = rest.as_bytes()[0];
    if sep != b'T' && sep != b' ' {
        return None;
    }
    let mut ymd = date.split('-');
    let (y, mo, d) = (
        digits(ymd.next()?)?,
        digits(ymd.next()?)?,
        digits(ymd.next()?)?,
    );
    if ymd.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let hms = &rest[1..9];
    let mut parts = hms.split(':');
    let (h, mi, sec) = (
        digits(parts.next()?)?,
        digits(parts.next()?)?,
        digits(parts.next()?)?,
    );
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut tail = &rest[9..];
    let mut ms = 0i64;
    if let Some(frac) = tail.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        let head: String = frac[..n].chars().chain("000".chars()).take(3).collect();
        ms = digits(&head)?;
        tail = &frac[n..];
    }
    let offset_min = match tail {
        "" | "Z" | "z" => 0,
        t if (t.starts_with('+') || t.starts_with('-')) && t.len() == 6 && &t[3..4] == ":" => {
            let sign = if t.starts_with('-') { -1 } else { 1 };
            sign * (digits(&t[1..3])? * 60 + digits(&t[4..6])?)
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    Some(secs * 1000 + ms)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for unix milliseconds, to the second.
pub fn iso_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ── pricing one turn ─────────────────────────────────────────────────────────

/// Which of a turn's pricing inputs it did not record, where the price depends on them. Each
/// makes a figure built on the turn a floor, as everywhere else in the app
/// ([`rates::Shortfall`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Unknowns {
    /// No `usage.speed` on a model whose fast mode costs more.
    pub speed: bool,
    /// No usable `usage.inference_geo` on a model where US-only inference costs more.
    pub geo: bool,
    /// A cache write with no split by duration.
    pub duration: bool,
}

impl Unknowns {
    pub fn or(self, o: Unknowns) -> Unknowns {
        Unknowns {
            speed: self.speed || o.speed,
            geo: self.geo || o.geo,
            duration: self.duration || o.duration,
        }
    }

    /// The app's own words for each, from [`rates::Shortfall::describe`].
    pub fn shortfalls(self) -> Vec<rates::Shortfall> {
        let mut out = Vec::new();
        if self.speed {
            out.push(rates::Shortfall::SpeedUnknown);
        }
        if self.geo {
            out.push(rates::Shortfall::GeoUnknown);
        }
        if self.duration {
            out.push(rates::Shortfall::DurationUnknown);
        }
        out
    }

    /// Short names, for a call line.
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.speed {
            out.push("speed");
        }
        if self.geo {
            out.push("region");
        }
        if self.duration {
            out.push("cacheDuration");
        }
        out
    }
}

/// A turn's prices at the floor and at the ceiling, through [`rates::rate_for`] with the speed
/// and region it recorded. They differ only where one was not recorded and matters: the floor is
/// what the rest of the app prices such a turn at (standard speed, global routing), the ceiling
/// fast mode's prices and US-only inference's.
fn prices_of(
    model: Option<&str>,
    speed: Option<&str>,
    geo: Option<&str>,
) -> Option<(Prices, Prices, Unknowns)> {
    let model = model?;
    let floor = rates::rate_for(model, speed, geo)?;
    let speed_hi = if floor.speed_unknown {
        Some("fast")
    } else {
        speed
    };
    let geo_hi = if floor.geo_unknown {
        Some(rates::GEO_US)
    } else {
        geo
    };
    let ceiling = match rates::rate_for(model, speed_hi, geo_hi) {
        Some(r) => r.prices,
        None => floor.prices,
    };
    let unknowns = Unknowns {
        speed: floor.speed_unknown,
        geo: floor.geo_unknown,
        duration: false,
    };
    Some((floor.prices, ceiling, unknowns))
}

/// A turn's model, speed and region: what it is priced by. Turns are grouped by it so each
/// price is looked up once, not once per turn.
struct Class<'a> {
    model: Option<&'a str>,
    speed: Option<&'a str>,
    geo: Option<&'a str>,
    prices: Option<(Prices, Prices, Unknowns)>,
}

/// One turn, as a chain holds it.
pub(super) struct Turn {
    pub ts_ms: i64,
    /// Breaks a tie in `ts_ms`: the rowid, or the order a hand-made row was given in.
    order: i64,
    class: u32,
    tokens: Tokens,
    split_recorded: bool,
    cache_5m: i64,
    cache_1h: i64,
}

impl Turn {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ts_ms: i64,
        order: i64,
        class: u32,
        tokens: [i64; 4],
        cache_5m: Option<i64>,
        cache_1h: Option<i64>,
    ) -> Self {
        let [input, output, cache_read, cache_write] = tokens;
        let split_recorded = cache_5m.is_some() || cache_1h.is_some();
        let (c5, c1) = match (cache_5m, cache_1h) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => (a, 0),
            (None, Some(b)) => (0, b),
            (None, None) => (0, 0),
        };
        Turn {
            ts_ms,
            order,
            class,
            tokens: Tokens::of_turn(
                input,
                output,
                cache_read,
                cache_write,
                split_recorded.then_some(c1),
            ),
            split_recorded,
            cache_5m: c5,
            cache_1h: c1,
        }
    }

    /// What the turn sent the model: every token of its prompt, cached or not.
    fn prompt(&self) -> i64 {
        self.tokens.input + self.tokens.cache_write + self.tokens.cache_read
    }

    /// The turn's cost at the floor (as the app prices it: writes of unrecorded duration at 5
    /// minutes) and at the ceiling (those writes at 1 hour, and the ceiling's prices).
    fn cost(&self, floor: &Prices, ceiling: &Prices) -> (f64, f64) {
        let mut as_1h = self.tokens;
        as_1h.cache_write_1h += as_1h.cache_write_unknown;
        as_1h.cache_write_unknown = 0;
        (
            floor.cost(&self.tokens).total(),
            ceiling.cost(&as_1h).total(),
        )
    }

    /// A cache write with no duration recorded, which prices at the 5-minute rate as a floor.
    fn duration_unknown(&self) -> bool {
        self.tokens.cache_write_unknown > 0
    }

    /// What putting one new token in context cost this turn, per million, at the floor and the
    /// ceiling, and the cache duration it was written at.
    fn one_time(&self, floor: &Prices, ceiling: &Prices) -> (f64, f64, Ttl) {
        if self.tokens.cache_write == 0 {
            return (floor.input, ceiling.input, Ttl::Uncached);
        }
        if !self.split_recorded {
            return (
                floor.cache_write_5m,
                ceiling.cache_write_1h,
                Ttl::Unrecorded,
            );
        }
        match (self.cache_5m > 0, self.cache_1h > 0) {
            (false, true) => (floor.cache_write_1h, ceiling.cache_write_1h, Ttl::OneHour),
            (true, false) => (
                floor.cache_write_5m,
                ceiling.cache_write_5m,
                Ttl::FiveMinutes,
            ),
            _ => (floor.cache_write_5m, ceiling.cache_write_1h, Ttl::Mixed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ttl {
    OneHour,
    FiveMinutes,
    Mixed,
    Unrecorded,
    Uncached,
}

impl Ttl {
    pub fn as_str(self) -> &'static str {
        match self {
            Ttl::OneHour => "1h",
            Ttl::FiveMinutes => "5m",
            Ttl::Mixed => "mixed",
            Ttl::Unrecorded => "unrecorded",
            Ttl::Uncached => "uncached",
        }
    }
}

/// What ended R.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bound {
    /// A compaction the daemon recorded in this conversation's transcript.
    Compaction,
    Rewind,
    /// Inferred: a later turn sent a smaller prompt than the turn before it. A conversation's
    /// prompt only grows, request by request, until something takes context out: a compaction,
    /// a rewind, tool results cleared, none of which the ledger need have recorded. Whether the
    /// result was what went cannot be told, so R ends there: never too long.
    Collapse,
    /// The end of the session, in a transcript the daemon was recording events for.
    SessionEnd,
    /// The end of the session as the turns show it, in a transcript from before the daemon
    /// recorded events, with no collapse to see. Every compaction shrinks the prompt, so none
    /// should hide here; but a turn the ledger lacks, or a rewind that came back with a prompt as
    /// large as the one it left, can hide one, and there is no event record to rule that out. R
    /// may be too long: the saving is measured at R = 0, and the rest is modelled.
    SessionEndUnrecorded,
    /// The session's latest turn is recent: it may still be running, and R is a lower bound.
    SessionOpen,
}

impl Bound {
    pub fn as_str(self) -> &'static str {
        match self {
            Bound::Compaction => "compaction",
            Bound::Rewind => "rewind",
            Bound::Collapse => "collapse",
            Bound::SessionEnd => "sessionEnd",
            Bound::SessionEndUnrecorded => "sessionEndUnrecorded",
            Bound::SessionOpen => "sessionOpen",
        }
    }
}

/// Why a row has no pair of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    NoSession,
    UnreadableTime,
    NoFollowingTurn,
    /// A compaction or rewind came between the call and the next turn.
    ClearedBeforeRead,
    /// The call names a subagent with no turns in the ledger.
    SubagentNotInLedger,
}

impl Why {
    pub fn as_str(self) -> &'static str {
        match self {
            Why::NoSession => "noSession",
            Why::UnreadableTime => "unreadableTime",
            Why::NoFollowingTurn => "noFollowingTurn",
            Why::ClearedBeforeRead => "clearedBeforeRead",
            Why::SubagentNotInLedger => "subagentNotInLedger",
        }
    }
}

/// Prices of one line: the value of a saved token (USD per token) and one extra round (USD), at
/// the floor and the ceiling, and the output price (USD per token) for inline text. The central
/// figure takes the value at the floor and the round at the ceiling, so the net it gives is a
/// floor wherever a price input was not recorded.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Price {
    pub v_lo: f64,
    pub v_hi: f64,
    /// The part of `v_lo` and `v_hi` that does not depend on R: putting the token in context
    /// once. What a saved token is worth at R = 0.
    pub once_lo: f64,
    pub once_hi: f64,
    pub c_lo: f64,
    pub c_hi: f64,
    pub out_lo: f64,
    pub out_hi: f64,
    pub some_rounds_unpriced: bool,
    /// What the turns behind these prices did not record.
    pub unknowns: Unknowns,
}

/// A pair derived from the line's own turn chain.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pair {
    pub model: Option<String>,
    pub speed: Option<String>,
    pub geo: Option<String>,
    pub rounds: i64,
    pub bound: Bound,
    /// R cannot be too long: a recorded event ended it, or the daemon was recording events for
    /// this conversation. False where an unrecorded compaction could have ended it sooner.
    pub rounds_trusted: bool,
    pub ttl: Ttl,
    /// `None` when the consuming turn's model has no price.
    pub price: Option<Price>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Outcome {
    Derived(Pair),
    /// No consuming turn to price it by: tokens only.
    NoPair(Why),
}

// ── chains ───────────────────────────────────────────────────────────────────

/// One conversation's turns in order, with running sums of what a cache read cost on each.
struct Chain {
    turns: Vec<Turn>,
    /// For each turn, the first later turn whose prompt is smaller than the turn before it
    /// (`turns.len()` when none): where context was first taken out after it.
    next_drop: Vec<u32>,
    /// The daemon was recording compactions and rewinds when this conversation began.
    recorded: bool,
    /// `read_lo[i]` is the sum over turns `0..i` of the floor cache-read price, per million.
    read_lo: Vec<f64>,
    read_hi: Vec<f64>,
    /// Turns `0..i` with no price.
    unpriced: Vec<u32>,
    /// Turns `0..i` whose speed, and whose region, went unrecorded where it matters.
    speed_unknown: Vec<u32>,
    geo_unknown: Vec<u32>,
}

impl Chain {
    fn new(mut turns: Vec<Turn>, classes: &[Class<'_>]) -> Self {
        // Turns of the same instant in the order they were written.
        turns.sort_by_key(|t| (t.ts_ms, t.order));
        let n = turns.len() + 1;
        // The first shrink after each turn, by one pass from the end.
        let mut next_drop = vec![turns.len() as u32; turns.len()];
        for j in (1..turns.len()).rev() {
            next_drop[j - 1] = if turns[j].prompt() < turns[j - 1].prompt() {
                j as u32
            } else {
                next_drop[j]
            };
        }
        let mut c = Chain {
            next_drop,
            recorded: false,
            read_lo: Vec::with_capacity(n),
            read_hi: Vec::with_capacity(n),
            unpriced: Vec::with_capacity(n),
            speed_unknown: Vec::with_capacity(n),
            geo_unknown: Vec::with_capacity(n),
            turns: Vec::new(),
        };
        let (mut lo, mut hi, mut none, mut speed, mut geo) = (0.0, 0.0, 0u32, 0u32, 0u32);
        for t in &turns {
            c.read_lo.push(lo);
            c.read_hi.push(hi);
            c.unpriced.push(none);
            c.speed_unknown.push(speed);
            c.geo_unknown.push(geo);
            match &classes[t.class as usize].prices {
                Some((f, ceiling, u)) => {
                    lo += f.cache_read;
                    hi += ceiling.cache_read;
                    speed += u32::from(u.speed);
                    geo += u32::from(u.geo);
                }
                None => none += 1,
            }
        }
        c.read_lo.push(lo);
        c.read_hi.push(hi);
        c.unpriced.push(none);
        c.speed_unknown.push(speed);
        c.geo_unknown.push(geo);
        c.turns = turns;
        c
    }
}

/// A conversation: the session, and the subagent inside it (`None` for the main agent).
type Key<'a> = (&'a str, Option<&'a str>);

/// A turn's model, speed and region, as written.
type Named<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>);

/// The subagent a transcript belongs to. Claude Code writes a subagent's transcript to
/// `~/.claude/projects/<project>/<session>/subagents/agent-<agent_id>.jsonl`, beside an
/// `agent-<agent_id>.meta.json`, and the hooks name the same `<agent_id>` (bare, as
/// `read_events.agent_id` holds it), so the file name is the key. Either separator: a ledger
/// written on Windows holds backslashes.
///
/// A path under `subagents/` is a subagent's even where the row says otherwise: the `LIKE`
/// backfill that marks old rows matches forward slashes only, and a subagent turn read as the
/// main agent's would add rounds to the wrong chain. A subagent turn whose file says nothing
/// usable gets an id no call carries, so it joins no chain a call reads.
pub(super) fn agent_of(source_file: Option<&str>, is_subagent: bool) -> Option<&str> {
    let sub = is_subagent || source_file.is_some_and(crate::project::is_subagent_path);
    if !sub {
        return None;
    }
    let id = source_file
        .and_then(|f| f.rsplit(['/', '\\']).next())
        .and_then(|name| name.strip_prefix("agent-"))
        .map(|name| name.strip_suffix(".jsonl").unwrap_or(name))
        .filter(|id| !id.is_empty());
    Some(id.unwrap_or(""))
}

pub(super) struct Chains<'a> {
    classes: Vec<Class<'a>>,
    chains: HashMap<Key<'a>, Chain>,
    bounds: HashMap<Key<'a>, Vec<(i64, Bound)>>,
    /// Each session's subagents with turns in the ledger.
    subagents: HashMap<&'a str, Vec<&'a str>>,
    now_ms: i64,
    pub unreadable_turns: usize,
}

impl<'a> Chains<'a> {
    pub fn build(ledger: &'a super::Ledger, now_ms: i64) -> Self {
        let mut classes: Vec<Class<'a>> = ledger
            .classes
            .iter()
            .map(|c| Class {
                model: c.model.as_deref(),
                speed: c.speed.as_deref(),
                geo: c.inference_geo.as_deref(),
                prices: prices_of(
                    c.model.as_deref(),
                    c.speed.as_deref(),
                    c.inference_geo.as_deref(),
                ),
            })
            .collect();
        let mut grouped: HashMap<Key<'a>, Vec<Turn>> = HashMap::new();
        let mut unreadable_turns = 0;
        for session in &ledger.sessions {
            // A session's turns are the main agent's, mostly, and a few subagents': split them
            // here rather than hash every turn into the map.
            let mut main: Vec<Turn> = Vec::with_capacity(session.turns.len());
            let mut subs: Vec<(&'a str, Vec<Turn>)> = Vec::new();
            for t in &session.turns {
                let Some(ts) = t.ts_ms else {
                    unreadable_turns += 1;
                    continue;
                };
                // As of `now_ms`, a later turn has not happened. A `<synthetic>` message is one
                // Claude Code wrote itself, not a request: it read nothing and is no round.
                if ts > now_ms || classes[t.class as usize].model == Some(SYNTHETIC) {
                    continue;
                }
                let turn = Turn::new(
                    ts,
                    t.id,
                    t.class,
                    [t.input, t.output, t.cache_read, t.cache_write],
                    t.cache_5m,
                    t.cache_1h,
                );
                match agent_of(t.source_file.as_deref(), t.is_subagent) {
                    None => main.push(turn),
                    Some(agent) => match subs.iter_mut().find(|(a, _)| *a == agent) {
                        Some((_, v)) => v.push(turn),
                        None => subs.push((agent, vec![turn])),
                    },
                }
            }
            let id = session.session_id.as_str();
            if !main.is_empty() {
                grouped.entry((id, None)).or_default().extend(main);
            }
            for (agent, v) in subs {
                grouped.entry((id, Some(agent))).or_default().extend(v);
            }
        }
        // Rows made by hand: their classes are named here, after the loaders'.
        let mut class_of: HashMap<Named<'a>, u32> = HashMap::new();
        for (n, row) in ledger.turns.iter().enumerate() {
            let Some(ts) = parse_ts_ms(&row.ts) else {
                unreadable_turns += 1;
                continue;
            };
            if ts > now_ms || row.model.as_deref() == Some(SYNTHETIC) {
                continue;
            }
            let what = (
                row.model.as_deref(),
                row.speed.as_deref(),
                row.inference_geo.as_deref(),
            );
            let class = *class_of.entry(what).or_insert_with(|| {
                classes.push(Class {
                    model: what.0,
                    speed: what.1,
                    geo: what.2,
                    prices: prices_of(what.0, what.1, what.2),
                });
                (classes.len() - 1) as u32
            });
            let key = (
                row.session_id.as_str(),
                agent_of(row.source_file.as_deref(), row.is_subagent),
            );
            grouped.entry(key).or_default().push(Turn::new(
                ts,
                n as i64,
                class,
                [row.input, row.output, row.cache_read, row.cache_write],
                row.cache_5m,
                row.cache_1h,
            ));
        }
        let events = &ledger.boundaries;
        let chains: HashMap<Key<'a>, Chain> = grouped
            .into_iter()
            .map(|(k, v)| {
                let mut c = Chain::new(v, &classes);
                // Recorded when the conversation began after the earliest event the daemon wrote
                // for its kind of transcript: see [`super::Ledger::events_from`].
                let from = match k.1 {
                    None => ledger.events_from,
                    Some(_) => ledger.subagent_events_from,
                };
                c.recorded = match (from, c.turns.first()) {
                    (Some(from), Some(first)) => first.ts_ms >= from,
                    _ => false,
                };
                (k, c)
            })
            .collect();
        let mut bounds: HashMap<Key<'a>, Vec<(i64, Bound)>> = HashMap::new();
        for e in events {
            // A resync is not an end: it says the tracker restarted mid-file, and what follows
            // in the file is still read for explicit compaction records, which need no state. A
            // rewind it may have missed shows as a collapse.
            let kind = match e.kind.as_str() {
                "compaction" | "subagent_compaction" => Bound::Compaction,
                "rewind" => Bound::Rewind,
                _ => continue,
            };
            if e.ts_ms > now_ms {
                continue;
            }
            let key = (
                e.session_id.as_str(),
                agent_of(e.source_file.as_deref(), false),
            );
            bounds.entry(key).or_default().push((e.ts_ms, kind));
        }
        for v in bounds.values_mut() {
            v.sort_by_key(|(t, _)| *t);
        }
        let mut subagents: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
        for (session, agent) in chains.keys() {
            if let Some(agent) = agent {
                subagents.entry(*session).or_default().push(*agent);
            }
        }
        for v in subagents.values_mut() {
            v.sort_unstable();
        }
        Chains {
            classes,
            chains,
            bounds,
            subagents,
            now_ms,
            unreadable_turns,
        }
    }

    /// The subagents of `session` with a turn from `from_ms` to `to_ms`: the agents other than the
    /// main one that were running then, in the order of their ids.
    pub fn subagents_active(&self, session: &str, from_ms: i64, to_ms: i64) -> Vec<&'a str> {
        let Some(agents) = self.subagents.get(session) else {
            return Vec::new();
        };
        agents
            .iter()
            .copied()
            .filter(|agent| {
                self.chains.get(&(session, Some(*agent))).is_some_and(|c| {
                    let i = c.turns.partition_point(|t| t.ts_ms < from_ms);
                    c.turns.get(i).is_some_and(|t| t.ts_ms <= to_ms)
                })
            })
            .collect()
    }

    fn named(&self, turn: &Turn) -> (Option<String>, Option<String>, Option<String>) {
        let c = &self.classes[turn.class as usize];
        (
            c.model.map(str::to_string),
            c.speed.map(str::to_string),
            c.geo.map(str::to_string),
        )
    }

    /// The pair for a call at `call_ms` (its recorded second) in `session`, by `agent`.
    pub fn derive(&self, session: &str, agent: Option<&str>, call_ms: i64) -> Outcome {
        let key = (session, agent);
        let Some(chain) = self.chains.get(&key) else {
            return Outcome::NoPair(match agent {
                Some(_) => Why::SubagentNotInLedger,
                None => Why::NoFollowingTurn,
            });
        };
        // The call's recorded second must be over: the turn that made the call can share it.
        let i = chain.turns.partition_point(|t| t.ts_ms < call_ms + 1000);
        let Some(consumer) = chain.turns.get(i) else {
            return Outcome::NoPair(Why::NoFollowingTurn);
        };
        // An event in the call's own second is taken to follow the call: the earlier end.
        let next_bound = self.bounds.get(&key).and_then(|b| {
            let j = b.partition_point(|(t, _)| *t < call_ms);
            b.get(j).copied()
        });
        if let Some((b_ms, _)) = next_bound
            && b_ms <= consumer.ts_ms
        {
            return Outcome::NoPair(Why::ClearedBeforeRead);
        }
        let recorded_end = match next_bound {
            Some((b_ms, _)) => chain.turns.partition_point(|t| t.ts_ms < b_ms),
            None => chain.turns.len(),
        };
        // The first later turn that sent a smaller prompt than the turn before it: context was
        // taken out there, and the result may have been what went. This ends R no later than the
        // first prompt smaller than the reading turn's, which the result cannot be in, and it
        // catches what that misses: a compaction that leaves more than the result found (a long
        // session's summary is larger than its first prompts), and cleared tool results.
        let collapse = chain.next_drop[i] as usize;
        let end = recorded_end.min(collapse);
        let rounds = end.saturating_sub(i + 1);
        let bound = if collapse < recorded_end {
            Bound::Collapse
        } else {
            match next_bound {
                Some((_, kind)) => kind,
                None => match chain.turns.last() {
                    Some(last) if last.ts_ms >= self.now_ms - super::OPEN_SESSION_MS => {
                        Bound::SessionOpen
                    }
                    _ if chain.recorded => Bound::SessionEnd,
                    _ => Bound::SessionEndUnrecorded,
                },
            }
        };
        // An event, recorded or seen as the first shrink, is the end, or ends R sooner: every
        // compaction shrinks the prompt. Only the end of a session from before the daemon recorded
        // events has nothing to vouch for it.
        let rounds_trusted = bound != Bound::SessionEndUnrecorded;
        let (model, speed, geo) = self.named(consumer);
        let mut pair = Pair {
            model,
            speed,
            geo,
            rounds: rounds as i64,
            bound,
            rounds_trusted,
            ttl: Ttl::Unrecorded,
            price: None,
        };
        if let Some((floor, ceiling, mut unknowns)) = self.classes[consumer.class as usize].prices {
            let (w_lo, w_hi, ttl) = consumer.one_time(&floor, &ceiling);
            let reads_lo = chain.read_lo[end] - chain.read_lo[i + 1];
            let reads_hi = chain.read_hi[end] - chain.read_hi[i + 1];
            let (c_lo, c_hi) = consumer.cost(&floor, &ceiling);
            // Of the consuming turn, and of the R turns its saving was re-read on.
            unknowns.duration = consumer.duration_unknown();
            unknowns.speed |= chain.speed_unknown[end] > chain.speed_unknown[i + 1];
            unknowns.geo |= chain.geo_unknown[end] > chain.geo_unknown[i + 1];
            let per_m = 1_000_000.0;
            pair.ttl = ttl;
            pair.price = Some(Price {
                v_lo: (w_lo + reads_lo) / per_m,
                v_hi: (w_hi + reads_hi) / per_m,
                once_lo: w_lo / per_m,
                once_hi: w_hi / per_m,
                c_lo,
                c_hi,
                out_lo: floor.output / per_m,
                out_hi: ceiling.output / per_m,
                some_rounds_unpriced: chain.unpriced[end] > chain.unpriced[i + 1],
                unknowns,
            });
        } else {
            let (_, _, ttl) = consumer.one_time(&ZERO, &ZERO);
            pair.ttl = ttl;
        }
        Outcome::Derived(pair)
    }

    /// The round a blocked Read cost: the main agent's latest turn at or before the retry the
    /// escape valve let through (the fault's recorded second). A fault records no agent, and the
    /// main agent's context is the larger, so a subagent's block is not priced low.
    pub fn retried_at(&self, session: &str, fault_ms: i64) -> Outcome {
        let Some(chain) = self.chains.get(&(session, None)) else {
            return Outcome::NoPair(Why::NoFollowingTurn);
        };
        let i = chain.turns.partition_point(|t| t.ts_ms < fault_ms + 1000);
        let Some(turn) = i.checked_sub(1).and_then(|j| chain.turns.get(j)) else {
            return Outcome::NoPair(Why::NoFollowingTurn);
        };
        let price =
            self.classes[turn.class as usize]
                .prices
                .map(|(floor, ceiling, mut unknowns)| {
                    let (c_lo, c_hi) = turn.cost(&floor, &ceiling);
                    unknowns.duration = turn.duration_unknown();
                    Price {
                        v_lo: 0.0,
                        v_hi: 0.0,
                        once_lo: 0.0,
                        once_hi: 0.0,
                        c_lo,
                        c_hi,
                        out_lo: 0.0,
                        out_hi: 0.0,
                        some_rounds_unpriced: false,
                        unknowns,
                    }
                });
        let (model, speed, geo) = self.named(turn);
        Outcome::Derived(Pair {
            model,
            speed,
            geo,
            rounds: 0,
            bound: Bound::SessionEnd,
            rounds_trusted: true,
            ttl: Ttl::Unrecorded,
            price,
        })
    }
}

/// The model Claude Code names on a message it wrote itself.
const SYNTHETIC: &str = "<synthetic>";

/// Prices of nothing, for naming a cache duration without a price to put on it.
const ZERO: Prices = Prices {
    input: 0.0,
    output: 0.0,
    cache_write_5m: 0.0,
    cache_write_1h: 0.0,
    cache_read: 0.0,
};
