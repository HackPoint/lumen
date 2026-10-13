use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Context window tiers, used ONLY when the model is unknown.
pub const TIERS: &[u64] = &[200_000, 500_000, 1_000_000];
pub const WARN_RATIO: f64 = 0.80;
pub const ALERT_RATIO: f64 = 0.95;

/// Published context windows per model, from the Claude model catalog.
///
/// This is the authoritative source: the window is a property of the MODEL, not
/// of how full the context happens to be. Inferring it from observed fill —
/// which is what Lumen did before — systematically under-reports, because a
/// 1M-window session that has only reached 267K looks exactly like a 500K one.
///
/// Matched by longest prefix so dated snapshots (`claude-haiku-4-5-20251001`)
/// resolve the same as their alias. Order does not matter; the longest match wins.
const MODEL_WINDOWS: &[(&str, u64)] = &[
    // Every current Opus / Sonnet / Fable / Mythos model is 1M.
    ("claude-fable-5", 1_000_000),
    ("claude-mythos-5", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    // Haiku is the only current model below 1M.
    ("claude-haiku-4-5", 200_000),
];

/// The published context window for `model`, or None if we don't recognise it.
///
/// Returning None for an unknown model is deliberate — better to fall back to
/// fill-based inference than to assert a window we cannot back up. Older models
/// (Opus 4.5, Sonnet 4.5 and earlier) deliberately have no entry.
pub fn model_window(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    MODEL_WINDOWS
        .iter()
        .filter(|(id, _)| m.starts_with(id))
        // Longest prefix wins, so a future "claude-opus-5-1m" style id cannot be
        // captured by a shorter entry that happens to also match.
        .max_by_key(|(id, _)| id.len())
        .map(|&(_, window)| window)
}

/// Smallest tier ≥ fill, or 1 M if fill exceeds all tiers.
///
/// Fill-based inference is a FALLBACK for unrecognised models. It can only ever
/// establish a lower bound — prefer [`resolve_window`], which asks the model first.
pub fn infer_window(fill: u64) -> u64 {
    TIERS
        .iter()
        .copied()
        .find(|&t| fill <= t)
        .unwrap_or(*TIERS.last().unwrap())
}

/// The context window to display for a session.
///
/// `peak_fill` must be the session's HIGH-WATER MARK, not its current fill:
/// `/compact` drops the fill sharply, and inferring from the momentary value
/// would shrink the window mid-session and make the gauge jump.
pub fn resolve_window(model: &str, peak_fill: u64) -> u64 {
    match model_window(model) {
        // A published window is authoritative — but never claim a window smaller
        // than a fill we have actually observed.
        Some(published) => published.max(infer_window(peak_fill)),
        None => infer_window(peak_fill),
    }
}

// ── Prices ───────────────────────────────────────────────────────────────────
//
// Every dollar figure Lumen shows is priced here, at the list price of the model that
// consumed the tokens, the speed it ran at and where it ran. Until 1.9.0 there was one set of
// rates, Opus-class, applied to every turn whatever its model, so a Sonnet 4.6 turn was priced
// at 1.7 times what it cost and a Haiku 4.5 turn at five times.
//
// A figure is never shown as more complete than it is. Tokens of a model with no price are
// left out and the figure says so; a turn whose speed was not recorded is priced at the
// standard rate on a model whose fast mode costs more, a turn whose region is not known at the
// global rate on a model where US-only inference costs more, and a cache write whose duration
// was not recorded at the 5-minute rate, and a figure holding any of them is a floor and says
// so.
//
// These are the first-party Claude API's prices, with no batch discount. A turn that recorded
// US-only inference (`usage.inference_geo` "us") costs [`US_ONLY`] times them on the models
// that offer it, and one that recorded "global" costs them. Claude on Bedrock and Vertex AI
// is priced by those platforms: a Bedrock id (`us.anthropic.…`) or a dated Vertex id
// (`…@20251101`) is not in this table and gets no dollar figure, but Vertex sends current
// models under their bare first-party ids, and those turns are priced here at first-party
// rates.

/// The day [`PRICES`] was last checked against [`Provider::source`].
pub const PRICES_AS_OF: &str = "2026-10-13";

/// What US-only inference multiplies every price of a model that offers it by: input, output,
/// both cache writes and cache reads, on top of fast mode's prices too.
///
/// From [`Provider::source`], "Data residency pricing": "For Claude 4.6 and later models,
/// specifying US-only inference through the `inference_geo` parameter incurs a 1.1x multiplier
/// on all token pricing categories, including input tokens, output tokens, cache writes, and
/// cache reads. … Global routing (the default) uses standard pricing." Its "Fast mode pricing"
/// says the data residency multipliers apply on top of fast mode's. Checked on 2026-10-14, with
/// <https://platform.claude.com/docs/en/manage-claude/data-residency>, which says the same and
/// that the older models do not take the parameter and always cost the standard price.
pub const US_ONLY: f64 = 1.1;

/// `usage.inference_geo` for a turn whose inference ran only in the US. It costs [`US_ONLY`]
/// times the price on a model whose [`PriceRow::us_only`] says so.
pub const GEO_US: &str = "us";

/// `usage.inference_geo` for a turn routed globally, at the standard price.
pub const GEO_GLOBAL: &str = "global";

/// What Claude Code writes in `usage.inference_geo` when it has no region to report: the real
/// transcript line in this crate's `lib.rs` tests carries it.
///
/// Neither page above says which price such a turn paid. They define "us" and "global" only,
/// and the Messages API reference types the field as "string or null" with no list of values
/// (<https://platform.claude.com/docs/en/api/messages/create>). Nor does a request that sent no
/// `inference_geo` mean global: the workspace's `default_inference_geo` applies, and a workspace
/// that had opted out of global routing was migrated to "us". So it is priced as global, the
/// lower of the two, and the figure holding it is a floor, the same as a turn that recorded
/// nothing.
pub const GEO_NOT_AVAILABLE: &str = "not_available";

/// Who sets a model's price.
///
/// Only Anthropic's first-party API for now. A provider is a variant rather than a string so
/// that adding one is a compile error at every place that has to say where its prices come
/// from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
}

impl Provider {
    /// Where this provider publishes the prices [`PRICES`] holds for it.
    pub const fn source(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://platform.claude.com/docs/en/about-claude/pricing",
        }
    }
}

/// One model's list prices at one speed, in USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    /// Writing to the 5-minute prompt cache.
    pub cache_write_5m: f64,
    /// Writing to the 1-hour prompt cache.
    pub cache_write_1h: f64,
    /// A cache hit or refresh.
    pub cache_read: f64,
}

/// A model in [`PRICES`]: its prices, or the reason it has none.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceRow {
    pub provider: Provider,
    /// The id with no snapshot date and no context suffix, as [`price_key`] gives it.
    pub model: &'static str,
    /// At standard speed.
    pub prices: Option<Prices>,
    /// In fast mode, for a model that has one. None for every other model, and a turn
    /// recorded as fast on one of those has no price.
    pub fast: Option<Prices>,
    /// What US-only inference multiplies this model's prices by, fast mode's included:
    /// [`US_ONLY`] on the 4.6 and later models. None on an older one, which does not take
    /// `inference_geo` and is billed at the standard price wherever it ran.
    pub us_only: Option<f64>,
    /// Where on the provider's page the prices were read, or why there are none.
    pub note: &'static str,
}

const fn usd([input, output, cache_write_5m, cache_write_1h, cache_read]: [f64; 5]) -> Prices {
    Prices {
        input,
        output,
        cache_write_5m,
        cache_write_1h,
        cache_read,
    }
}

// A row offers US-only inference unless it says it does not: every model from 4.6 on does, so
// a new one added without thinking about it gets the premium rather than losing it.
const fn priced(model: &'static str, standard: [f64; 5], note: &'static str) -> PriceRow {
    PriceRow {
        provider: Provider::Anthropic,
        model,
        prices: Some(usd(standard)),
        fast: None,
        us_only: Some(US_ONLY),
        note,
    }
}

const fn unpriced(model: &'static str, why: &'static str) -> PriceRow {
    PriceRow {
        provider: Provider::Anthropic,
        model,
        prices: None,
        fast: None,
        us_only: Some(US_ONLY),
        note: why,
    }
}

impl PriceRow {
    const fn with_fast(self, fast: [f64; 5]) -> PriceRow {
        PriceRow {
            fast: Some(usd(fast)),
            ..self
        }
    }

    /// A model older than 4.6, which does not take `inference_geo`: "Requests with
    /// `inference_geo` on Claude Opus 4.5, Claude Sonnet 4.5 (deprecated), Claude Haiku 4.5,
    /// or earlier models return a 400 error" (the data residency page).
    const fn without_inference_geo(self) -> PriceRow {
        PriceRow {
            us_only: None,
            ..self
        }
    }
}

/// List prices per model, from [`Provider::source`] as of [`PRICES_AS_OF`].
///
/// Each price is `[input, output, 5-minute cache write, 1-hour cache write, cache read]` in
/// USD per million tokens, copied from the "Model pricing" table on the source page; the note
/// names the table row it came from. A model that is missing, or listed without prices, gets
/// no dollar figure: its tokens are still counted, and every total they are left out of says
/// so. A price is never borrowed from a neighbouring model.
///
/// Fast mode: the page's "Fast mode pricing" table gives input and output for the three
/// models that have it, and says the prompt caching multipliers apply on top of them. The
/// cache prices in each `with_fast` are those multipliers on the fast input price: 1.25 and
/// 2 times for the two writes, and the model's own cache-hit multiplier (0.05 on Opus 5.5,
/// 0.1 on the others).
///
/// US-only inference: [`US_ONLY`] times every price, standard or fast, on every model from
/// 4.6 on. The three older models the page lists, Opus 4.5, Sonnet 4.5 and Haiku 4.5, are
/// `without_inference_geo`.
pub const PRICES: &[PriceRow] = &[
    // Cache hits on Fable 5.1 and Mythos 5.1 are 0.025 times input, not the usual 0.1.
    priced(
        "claude-fable-5-1",
        [10.0, 50.0, 12.5, 20.0, 0.25],
        "Model pricing, row Claude Fable 5.1",
    ),
    priced(
        "claude-mythos-5-1",
        [10.0, 50.0, 12.5, 20.0, 0.25],
        "Model pricing, row Claude Mythos 5.1",
    ),
    priced(
        "claude-fable-5",
        [10.0, 50.0, 12.5, 20.0, 1.0],
        "Model pricing, row Claude Fable 5",
    ),
    priced(
        "claude-mythos-5",
        [10.0, 50.0, 12.5, 20.0, 1.0],
        "Model pricing, row Claude Mythos 5",
    ),
    // Cache hits on Opus 5.5 and Sonnet 5.5 are 0.05 times input.
    priced(
        "claude-opus-5-5",
        [4.0, 20.0, 5.0, 8.0, 0.2],
        "Model pricing, row Claude Opus 5.5; fast mode, row Claude Opus 5.5",
    )
    .with_fast([8.0, 40.0, 10.0, 16.0, 0.4]),
    priced(
        "claude-opus-5",
        [5.0, 25.0, 6.25, 10.0, 0.5],
        "Model pricing, row Claude Opus 5; fast mode, row Claude Opus 5 / Claude Opus 4.8",
    )
    .with_fast([10.0, 50.0, 12.5, 20.0, 1.0]),
    priced(
        "claude-opus-4-8",
        [5.0, 25.0, 6.25, 10.0, 0.5],
        "Model pricing, row Claude Opus 4.8; fast mode, row Claude Opus 5 / Claude Opus 4.8",
    )
    .with_fast([10.0, 50.0, 12.5, 20.0, 1.0]),
    // Fast mode is not available on Opus 4.7, and on Opus 4.6 a fast request runs and is
    // billed at standard speed and recorded as "standard".
    priced(
        "claude-opus-4-7",
        [5.0, 25.0, 6.25, 10.0, 0.5],
        "Model pricing, row Claude Opus 4.7",
    ),
    priced(
        "claude-opus-4-6",
        [5.0, 25.0, 6.25, 10.0, 0.5],
        "Model pricing, row Claude Opus 4.6",
    ),
    priced(
        "claude-opus-4-5",
        [5.0, 25.0, 6.25, 10.0, 0.5],
        "Model pricing, row Claude Opus 4.5",
    )
    .without_inference_geo(),
    // The page gives $0.10 for a Sonnet 5.5 cache hit (0.05 times its $2 input). A cached
    // copy of the model list elsewhere said $0.20; the page is the source and wins.
    priced(
        "claude-sonnet-5-5",
        [2.0, 10.0, 2.5, 4.0, 0.1],
        "Model pricing, row Claude Sonnet 5.5",
    ),
    // $2 / $10 began as introductory pricing and is now the standard price (page footnote 3).
    priced(
        "claude-sonnet-5",
        [2.0, 10.0, 2.5, 4.0, 0.2],
        "Model pricing, row Claude Sonnet 5",
    ),
    priced(
        "claude-sonnet-4-6",
        [3.0, 15.0, 3.75, 6.0, 0.3],
        "Model pricing, row Claude Sonnet 4.6",
    ),
    priced(
        "claude-haiku-4-5",
        [1.0, 5.0, 1.25, 2.0, 0.1],
        "Model pricing, row Claude Haiku 4.5",
    )
    .without_inference_geo(),
    unpriced(
        "claude-haiku-5-5",
        "Priced by prompt length: a request over 100,000 prompt tokens pays five times the \
         rate of one under it, and a total summed over many requests cannot say which paid \
         which.",
    ),
    unpriced(
        "claude-sonnet-4-5",
        "The page lists $3 / $15, but long-context pricing is standard only from the 4.6 \
         models on, and the premium a Sonnet 4.5 request over 200,000 tokens paid is no \
         longer on it.",
    )
    .without_inference_geo(),
];

/// The id `model` is priced under: lowercased, with a context suffix such as `[1m]` and a
/// snapshot date such as `-20251001` taken off.
///
/// Not the longest-prefix match [`model_window`] uses. A prefix would price
/// `claude-opus-5-5` as `claude-opus-5` the day its row went missing, and a future
/// `claude-opus-5-7` at an older model's rate. A window guessed that way is corrected by
/// the fill actually observed; a price has nothing to correct it.
pub fn price_key(model: &str) -> String {
    let mut key = model.trim().to_ascii_lowercase();
    if key.ends_with(']')
        && let Some(open) = key.rfind('[')
    {
        key.truncate(open);
    }
    if let Some((alias, date)) = key.rsplit_once('-')
        && date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
    {
        key.truncate(alias.len());
    }
    key
}

fn row_for(model: &str) -> Option<&'static PriceRow> {
    let key = price_key(model);
    PRICES.iter().find(|row| row.model == key)
}

/// A model's list prices at standard speed, or `None` when it has none.
///
/// `None` for a model not in [`PRICES`], for one listed without prices, and for
/// `<synthetic>`, which Claude Code writes for messages no model produced.
pub fn price_for(model: &str) -> Option<Prices> {
    row_for(model).and_then(|row| row.prices)
}

/// The prices a turn's tokens are priced at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub prices: Prices,
    /// The turn recorded no speed and the model has a fast mode, which costs more. `prices`
    /// are the standard ones, so what they price is a floor.
    pub speed_unknown: bool,
    /// Where the turn ran is not known and the model offers US-only inference, which costs
    /// more. `prices` are the global ones, so what they price is a floor.
    pub geo_unknown: bool,
}

/// What a turn of `model` that ran at `speed` in `geo` (`usage.speed` and
/// `usage.inference_geo` as recorded, `None` when they were not) is priced at, or `None` when
/// it cannot be priced.
///
/// A recorded `"fast"` takes the model's fast-mode prices, and has none on a model without
/// them. An unrecorded speed takes the standard prices, flagged as a floor where fast mode
/// exists. A speed this build does not know has no price: it may be a mode with a premium.
///
/// On a model that offers US-only inference, a recorded [`GEO_US`] costs [`US_ONLY`] times
/// those prices and [`GEO_GLOBAL`] the prices themselves. No region, or
/// [`GEO_NOT_AVAILABLE`], takes the global prices, flagged as a floor. A region this build
/// does not know has no price, as an unknown speed has none. A model that does not offer it
/// costs its prices wherever it ran.
pub fn rate_for(model: &str, speed: Option<&str>, geo: Option<&str>) -> Option<Rate> {
    let row = row_for(model)?;
    let (prices, speed_unknown) = match speed {
        Some("standard") => (row.prices, false),
        Some("fast") => (row.fast, false),
        Some(_) => (None, false),
        None => (row.prices, row.fast.is_some()),
    };
    let (multiplier, geo_unknown) = match (row.us_only, geo) {
        (None, _) => (1.0, false),
        (Some(us_only), Some(GEO_US)) => (us_only, false),
        (Some(_), Some(GEO_GLOBAL)) => (1.0, false),
        (Some(_), None | Some(GEO_NOT_AVAILABLE)) => (1.0, true),
        (Some(_), Some(_)) => return None,
    };
    prices.map(|prices| Rate {
        prices: prices.times(multiplier),
        speed_unknown,
        geo_unknown,
    })
}

/// Whether `geo` is a value [`rate_for`] prices, so a turn with no price can be named for the
/// region that has none.
fn known_geo(geo: &str) -> bool {
    [GEO_US, GEO_GLOBAL, GEO_NOT_AVAILABLE].contains(&geo)
}

/// Tokens of each kind, as one model consumed them at one speed in one region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    /// Every cache write, whatever its duration.
    pub cache_write: i64,
    /// The part of `cache_write` the transcript recorded as 1-hour writes.
    #[serde(default)]
    pub cache_write_1h: i64,
    /// The part of `cache_write` whose duration was not recorded: every turn ingested before
    /// 1.9.0. It is priced at the 5-minute rate, the lower of the two, so a cost holding any
    /// of it is a floor.
    #[serde(default)]
    pub cache_write_unknown: i64,
}

impl Tokens {
    /// A turn's tokens, from what its usage recorded. `cache_write_1h` is `None` when the
    /// transcript recorded no split of the cache write by duration, and then the whole write
    /// is of unknown duration.
    pub fn of_turn(
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        cache_write_1h: Option<i64>,
    ) -> Tokens {
        let (cache_write_1h, cache_write_unknown) = match cache_write_1h {
            Some(one_hour) => (one_hour, 0),
            None => (0, cache_write),
        };
        Tokens {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
            cache_write_unknown,
        }
    }

    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_write_unknown += other.cache_write_unknown;
    }

    /// Every token, of every kind.
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// What some tokens cost, by kind, in USD.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Cost {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: &Cost) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

const PER_MILLION: f64 = 1_000_000.0;

impl Prices {
    /// Every price times `k`.
    pub fn times(self, k: f64) -> Prices {
        Prices {
            input: self.input * k,
            output: self.output * k,
            cache_write_5m: self.cache_write_5m * k,
            cache_write_1h: self.cache_write_1h * k,
            cache_read: self.cache_read * k,
        }
    }

    /// What `t` cost at these prices.
    ///
    /// Cache writes the transcript recorded as 1-hour writes are priced at the 1-hour rate,
    /// and every other write at the 5-minute rate, including those whose duration was not
    /// recorded ([`Tokens::cache_write_unknown`]), which can therefore be low.
    pub fn cost(&self, t: &Tokens) -> Cost {
        let usd = |tokens: i64, per_million: f64| tokens as f64 * per_million / PER_MILLION;
        Cost {
            input: usd(t.input, self.input),
            output: usd(t.output, self.output),
            cache_read: usd(t.cache_read, self.cache_read),
            cache_write: usd(t.cache_write - t.cache_write_1h, self.cache_write_5m)
                + usd(t.cache_write_1h, self.cache_write_1h),
        }
    }

    /// What reading `cache_read` tokens from the prompt cache saved against sending them
    /// as fresh input. Claude Code's caching did that, not Lumen.
    pub fn caching_savings(&self, cache_read: i64) -> f64 {
        cache_read as f64 * (self.input - self.cache_read) / PER_MILLION
    }
}

/// Tokens by the model that consumed them, the speed it ran at and the region it ran in, so
/// each part is priced at its own rate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spend {
    by_rate: BTreeMap<(String, Option<String>, Option<String>), Tokens>,
}

impl Spend {
    /// Count `t` against `model` at `speed` in `geo` (as recorded; `None` when they were
    /// not). A model with no price, an empty one included, is kept under its own id, so its
    /// tokens stay in the totals that cannot be priced.
    pub fn add(&mut self, model: &str, speed: Option<&str>, geo: Option<&str>, t: Tokens) {
        self.by_rate
            .entry((
                model.to_string(),
                speed.map(str::to_string),
                geo.map(str::to_string),
            ))
            .or_default()
            .add(&t);
    }

    /// Every model's tokens together.
    pub fn tokens(&self) -> Tokens {
        let mut all = Tokens::default();
        for t in self.by_rate.values() {
            all.add(t);
        }
        all
    }

    /// Each part priced at its own model's rates, speed and region.
    pub fn priced(&self) -> Priced {
        let mut out = Priced::default();
        for ((model, speed, geo), t) in &self.by_rate {
            match rate_for(model, speed.as_deref(), geo.as_deref()) {
                Some(rate) => {
                    out.cost.add(&rate.prices.cost(t));
                    out.caching_savings += rate.prices.caching_savings(t.cache_read);
                    out.priced_tokens += t.total();
                    out.duration_unknown += t.cache_write_unknown;
                    if rate.speed_unknown {
                        out.speed_unknown.add(t);
                    }
                    if rate.geo_unknown {
                        out.geo_unknown.add(t);
                    }
                }
                None if t.total() != 0 => {
                    out.unpriced.add(t);
                    let mut name = match speed.as_deref() {
                        None | Some("standard") => model.clone(),
                        Some(s) => format!("{model} at speed {s}"),
                    };
                    if let Some(g) = geo.as_deref().filter(|g| !known_geo(g)) {
                        name.push_str(&format!(" in region {g}"));
                    }
                    if !out.unpriced_models.contains(&name) {
                        out.unpriced_models.push(name);
                    }
                }
                None => {}
            }
        }
        out
    }
}

/// A [`Spend`] in dollars, and every way the dollars can fall short of the whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Priced {
    pub cost: Cost,
    pub caching_savings: f64,
    pub priced_tokens: i64,
    /// Tokens of a model, a speed or a region with no price, by kind. In no dollar figure.
    pub unpriced: Tokens,
    /// Which models those are; '' for turns that recorded none.
    pub unpriced_models: Vec<String>,
    /// Tokens priced at standard rates whose speed was not recorded, on models whose fast
    /// mode costs more, by kind.
    pub speed_unknown: Tokens,
    /// Tokens priced at global rates whose region is not known, on models where US-only
    /// inference costs more, by kind.
    pub geo_unknown: Tokens,
    /// Cache writes priced at the 5-minute rate whose duration was not recorded.
    pub duration_unknown: i64,
}

/// A figure drawn from a [`Priced`]: one kind of token's cost, the total, or the caching
/// savings. Each can fall short for different reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Figure {
    Total,
    Input,
    Output,
    CacheRead,
    CacheWrite,
    /// What the prompt cache saved; depends on the read and input prices only.
    CachingSavings,
}

/// Why a dollar figure can be lower than what was really spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shortfall {
    /// These models had tokens in the figure's scope and no price.
    NoPrice(Vec<String>),
    /// Turns that recorded no speed, on a model whose fast mode costs more, were priced at
    /// the standard rate.
    SpeedUnknown,
    /// Turns whose region is not known, on a model where US-only inference costs more, were
    /// priced at the global rate.
    GeoUnknown,
    /// Cache writes that recorded no duration were priced at the 5-minute rate.
    DurationUnknown,
}

impl Shortfall {
    /// One line saying what was left out or priced low, and why.
    pub fn describe(&self) -> String {
        match self {
            Shortfall::NoPrice(models) => {
                let names: Vec<&str> = models
                    .iter()
                    .map(|m| {
                        if m.is_empty() {
                            "turns with no model"
                        } else {
                            m
                        }
                    })
                    .collect();
                format!("no price for {}", names.join(", "))
            }
            Shortfall::SpeedUnknown => {
                "speed not recorded: priced at the standard rate, below fast mode's".into()
            }
            Shortfall::GeoUnknown => format!(
                "inference region not known: priced at the global rate, below US-only \
                 inference's {US_ONLY} times"
            ),
            Shortfall::DurationUnknown => {
                "cache writes with no recorded duration: priced at the 5-minute rate".into()
            }
        }
    }
}

impl Priced {
    /// Some tokens are in no dollar figure.
    pub fn is_partial(&self) -> bool {
        self.unpriced.total() > 0
    }

    /// There is a figure to show: something was priced, or there was nothing to price.
    pub fn has_figure(&self) -> bool {
        self.priced_tokens > 0 || self.unpriced.total() == 0
    }

    /// The figure's dollars.
    pub fn amount(&self, figure: Figure) -> f64 {
        match figure {
            Figure::Total => self.cost.total(),
            Figure::Input => self.cost.input,
            Figure::Output => self.cost.output,
            Figure::CacheRead => self.cost.cache_read,
            Figure::CacheWrite => self.cost.cache_write,
            Figure::CachingSavings => self.caching_savings,
        }
    }

    /// Every reason `figure` can be lower than the truth. Empty when it is exact.
    pub fn shortfalls(&self, figure: Figure) -> Vec<Shortfall> {
        // The kinds of token each figure is made of.
        let in_scope = |t: &Tokens| -> i64 {
            match figure {
                Figure::Total => t.total(),
                Figure::Input => t.input,
                Figure::Output => t.output,
                Figure::CacheRead | Figure::CachingSavings => t.cache_read,
                Figure::CacheWrite => t.cache_write,
            }
        };
        let mut out = Vec::new();
        if in_scope(&self.unpriced) > 0 {
            out.push(Shortfall::NoPrice(self.unpriced_models.clone()));
        }
        if in_scope(&self.speed_unknown) > 0 {
            out.push(Shortfall::SpeedUnknown);
        }
        if in_scope(&self.geo_unknown) > 0 {
            out.push(Shortfall::GeoUnknown);
        }
        if self.duration_unknown > 0 && matches!(figure, Figure::Total | Figure::CacheWrite) {
            out.push(Shortfall::DurationUnknown);
        }
        out
    }

    /// `figure` is a floor: the real figure can be higher.
    pub fn is_floor(&self, figure: Figure) -> bool {
        !self.shortfalls(figure).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_window_small_fits_first_tier() {
        assert_eq!(infer_window(0), 200_000);
        assert_eq!(infer_window(1), 200_000);
        assert_eq!(infer_window(199_999), 200_000);
        assert_eq!(infer_window(200_000), 200_000);
    }

    #[test]
    fn infer_window_mid_tier() {
        assert_eq!(infer_window(200_001), 500_000);
        assert_eq!(infer_window(500_000), 500_000);
    }

    #[test]
    fn infer_window_large_tier() {
        assert_eq!(infer_window(500_001), 1_000_000);
        assert_eq!(infer_window(1_000_000), 1_000_000);
    }

    #[test]
    fn infer_window_over_max_clamps_to_last_tier() {
        assert_eq!(infer_window(1_000_001), 1_000_000);
        assert_eq!(infer_window(u64::MAX / 2), 1_000_000);
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    // ── model_window: the published windows ──────────────────────────────────

    #[test]
    fn every_current_opus_and_sonnet_is_one_million() {
        for id in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "claude-mythos-5",
        ] {
            assert_eq!(model_window(id), Some(1_000_000), "{id}");
        }
    }

    #[test]
    fn haiku_is_two_hundred_thousand() {
        assert_eq!(model_window("claude-haiku-4-5"), Some(200_000));
    }

    #[test]
    fn a_dated_snapshot_resolves_like_its_alias() {
        // The daemon records whatever Claude Code wrote, which includes dated ids.
        assert_eq!(
            model_window("claude-haiku-4-5-20251001"),
            Some(200_000),
            "dated haiku must not fall through to inference"
        );
    }

    #[test]
    fn model_matching_is_case_insensitive() {
        assert_eq!(model_window("Claude-Opus-4-8"), Some(1_000_000));
    }

    #[test]
    fn an_unknown_model_has_no_published_window() {
        // Deliberate: we do not assert a window we cannot back up.
        for id in [
            "",
            "<synthetic>",
            "gpt-4",
            "claude-opus-4-5-20251101",
            "llama",
        ] {
            assert_eq!(model_window(id), None, "{id}");
        }
    }

    // ── resolve_window: what the gauge actually shows ────────────────────────

    #[test]
    fn a_known_model_reports_its_published_window_regardless_of_fill() {
        // THE BUG: a 267K fill on a 1M-window model used to display as 500K,
        // because the window was inferred from the fill instead of the model.
        assert_eq!(resolve_window("claude-opus-4-8", 267_593), 1_000_000);
        assert_eq!(resolve_window("claude-opus-4-8", 0), 1_000_000);
        assert_eq!(resolve_window("claude-opus-4-8", 999_999), 1_000_000);
    }

    #[test]
    fn every_model_in_the_real_dataset_now_resolves_correctly() {
        // Peak fills observed in a real lumen.db, with the window each one used
        // to display versus what it must display now.
        let cases = [
            ("claude-opus-4-8", 457_136u64, 1_000_000u64),
            ("claude-sonnet-4-6", 298_606, 1_000_000),
            ("claude-opus-4-7", 172_040, 1_000_000),
            ("claude-opus-4-6", 166_470, 1_000_000),
            ("claude-haiku-4-5-20251001", 152_423, 200_000),
        ];
        for (model, peak, expected) in cases {
            assert_eq!(resolve_window(model, peak), expected, "{model}");
        }
    }

    #[test]
    fn haiku_stays_at_two_hundred_thousand_rather_than_inflating() {
        // Haiku's real ceiling is 200K; a busy session must not silently become 1M.
        assert_eq!(resolve_window("claude-haiku-4-5", 150_000), 200_000);
        assert_eq!(resolve_window("claude-haiku-4-5", 199_999), 200_000);
    }

    #[test]
    fn an_unknown_model_falls_back_to_fill_inference() {
        assert_eq!(resolve_window("<synthetic>", 0), 200_000);
        assert_eq!(resolve_window("mystery-model", 250_000), 500_000);
        assert_eq!(resolve_window("mystery-model", 900_000), 1_000_000);
    }

    #[test]
    fn an_observed_fill_above_the_published_window_widens_it() {
        // Should not happen, but if the API reports more fill than the published
        // window allows, believe the observation rather than showing over 100%.
        assert_eq!(
            resolve_window("claude-haiku-4-5", 400_000),
            500_000,
            "trust the measurement over the table"
        );
    }

    #[test]
    fn the_window_never_shrinks_as_fill_grows() {
        // Monotonic in peak_fill for every model, known or not — this is what
        // stops the gauge jumping around mid-session.
        for model in ["claude-opus-4-8", "claude-haiku-4-5", "unknown-model"] {
            let mut previous = 0;
            for peak in [0u64, 1_000, 199_999, 200_001, 499_999, 500_001, 2_000_000] {
                let w = resolve_window(model, peak);
                assert!(
                    w >= previous,
                    "{model} shrank at peak {peak}: {w} < {previous}"
                );
                previous = w;
            }
        }
    }

    #[test]
    fn a_compaction_drop_does_not_change_the_window() {
        // The caller passes the session PEAK, so a post-compaction fill of 16K
        // after a peak of 317K still resolves to the same window. This is the
        // non-monotonic fill pattern seen in real data (317241 -> 16794 -> 314173).
        let peak = 317_241;
        assert_eq!(
            resolve_window("claude-opus-4-8", peak),
            resolve_window("claude-opus-4-8", peak),
        );
        // And for an unknown model, the peak is what keeps it stable:
        assert_eq!(resolve_window("mystery", peak), 500_000);
        assert_ne!(
            resolve_window("mystery", 16_794),
            500_000,
            "passing the momentary fill instead of the peak is what caused the jump"
        );
    }
}

#[cfg(test)]
mod price_tests {
    use super::*;

    const STD: Option<&str> = Some("standard");
    const GLOBAL: Option<&str> = Some(GEO_GLOBAL);
    const US: Option<&str> = Some(GEO_US);

    fn prices(input: f64, output: f64, w5m: f64, w1h: f64, read: f64) -> Prices {
        Prices {
            input,
            output,
            cache_write_5m: w5m,
            cache_write_1h: w1h,
            cache_read: read,
        }
    }

    fn opus_4() -> Prices {
        prices(5.0, 25.0, 6.25, 10.0, 0.5)
    }

    fn tokens(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Tokens {
        Tokens {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h: 0,
            cache_write_unknown: 0,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ── the table ────────────────────────────────────────────────────────────

    #[test]
    fn every_model_with_a_window_has_a_price_or_a_reason_for_none() {
        // A model the gauge knows but the price table does not would show a window and no
        // cost, with nothing saying why. Every window entry needs a row: priced, or unpriced
        // with the reason written down.
        let mut missing = Vec::new();
        for &(id, _) in MODEL_WINDOWS {
            match PRICES.iter().find(|r| r.model == id) {
                Some(row) if row.prices.is_some() => {
                    assert!(price_for(id).is_some(), "{id} has a row but no price");
                }
                Some(row) => assert!(
                    !row.note.trim().is_empty(),
                    "{id} is unpriced without a reason"
                ),
                None => missing.push(id),
            }
        }
        assert!(missing.is_empty(), "no price row for {missing:?}");
    }

    #[test]
    fn every_row_says_where_its_price_came_from_or_why_it_has_none() {
        for row in PRICES {
            assert!(!row.note.trim().is_empty(), "{} has no note", row.model);
        }
    }

    #[test]
    fn every_row_is_keyed_by_its_own_price_key_once() {
        // A row keyed by a dated id or in capitals could never be found.
        let mut seen = std::collections::BTreeSet::new();
        for row in PRICES {
            assert_eq!(
                price_key(row.model),
                row.model,
                "{} is not canonical",
                row.model
            );
            assert!(seen.insert(row.model), "{} is listed twice", row.model);
        }
    }

    #[test]
    fn a_cache_hit_is_never_dearer_than_fresh_input_and_a_write_never_cheaper() {
        for row in PRICES {
            let Some(p) = row.prices else { continue };
            assert!(p.cache_read < p.input, "{}", row.model);
            assert!(p.cache_write_5m > p.input, "{}", row.model);
            assert!(p.cache_write_1h > p.cache_write_5m, "{}", row.model);
            assert!(p.output > p.input, "{}", row.model);
        }
    }

    // ── each model at its own rates ──────────────────────────────────────────

    #[test]
    fn sonnet_is_priced_at_sonnet_rates() {
        assert_eq!(
            price_for("claude-sonnet-4-6"),
            Some(prices(3.0, 15.0, 3.75, 6.0, 0.3))
        );
        assert_eq!(
            price_for("claude-sonnet-5"),
            Some(prices(2.0, 10.0, 2.5, 4.0, 0.2))
        );
        assert_eq!(
            price_for("claude-sonnet-5-5"),
            Some(prices(2.0, 10.0, 2.5, 4.0, 0.1))
        );
    }

    #[test]
    fn haiku_is_priced_at_haiku_rates() {
        assert_eq!(
            price_for("claude-haiku-4-5-20251001"),
            Some(prices(1.0, 5.0, 1.25, 2.0, 0.1))
        );
    }

    #[test]
    fn opus_is_priced_at_opus_rates() {
        for id in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-opus-4-5-20251101",
        ] {
            assert_eq!(price_for(id), Some(opus_4()), "{id}");
        }
        assert_eq!(
            price_for("claude-opus-5-5"),
            Some(prices(4.0, 20.0, 5.0, 8.0, 0.2)),
            "Opus 5.5 is cheaper than the Opus 5 its id starts with"
        );
    }

    #[test]
    fn fable_is_priced_at_fable_rates() {
        assert_eq!(
            price_for("claude-fable-5"),
            Some(prices(10.0, 50.0, 12.5, 20.0, 1.0))
        );
    }

    #[test]
    fn every_model_in_a_real_ledger_resolves_as_expected() {
        // The model ids a real lumen.db holds, with the input price each must get.
        let cases = [
            ("claude-opus-5", Some(5.0)),
            ("claude-opus-5-5", Some(4.0)),
            ("claude-opus-4-8", Some(5.0)),
            ("claude-sonnet-5-5", Some(2.0)),
            ("claude-haiku-4-5-20251001", Some(1.0)),
            ("claude-sonnet-4-6", Some(3.0)),
            ("claude-opus-4-7", Some(5.0)),
            ("claude-fable-5", Some(10.0)),
            ("claude-sonnet-5", Some(2.0)),
            ("claude-opus-4-6", Some(5.0)),
            ("claude-opus-4-5-20251101", Some(5.0)),
            ("<synthetic>", None),
        ];
        for (id, input) in cases {
            assert_eq!(price_for(id).map(|p| p.input), input, "{id}");
        }
    }

    // ── what does not match ──────────────────────────────────────────────────

    #[test]
    fn an_unknown_model_has_no_price() {
        for id in [
            "",
            "<synthetic>",
            "gpt-4",
            "claude-opus-5-7",
            "claude-opus-4-8-fast",
            "claude-opus-5-55",
            "us.anthropic.claude-opus-4-8-v1:0",
            "claude-opus-4-5@20251101",
        ] {
            assert_eq!(price_for(id), None, "{id}");
        }
    }

    #[test]
    fn a_model_listed_as_unpriced_has_no_price() {
        assert_eq!(price_for("claude-haiku-5-5"), None);
        assert_eq!(price_for("claude-sonnet-4-5-20250929"), None);
    }

    #[test]
    fn a_context_suffix_a_snapshot_date_and_case_do_not_change_the_price() {
        assert_eq!(price_for("claude-opus-4-8[1m]"), Some(opus_4()));
        assert_eq!(price_for("Claude-Opus-4-8"), Some(opus_4()));
        assert_eq!(price_key("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(price_key("claude-sonnet-4-6[1m]"), "claude-sonnet-4-6");
        // Only an eight-digit tail is a date: "-5" is part of the model's name.
        assert_eq!(price_key("claude-opus-5-5"), "claude-opus-5-5");
    }

    // ── cost ─────────────────────────────────────────────────────────────────

    #[test]
    fn each_kind_of_token_is_priced_at_its_own_rate() {
        let c = opus_4().cost(&tokens(1_000_000, 1_000_000, 1_000_000, 1_000_000));
        assert!(close(c.input, 5.0), "{c:?}");
        assert!(close(c.output, 25.0), "{c:?}");
        assert!(close(c.cache_read, 0.5), "{c:?}");
        assert!(close(c.cache_write, 6.25), "{c:?}");
        assert!(close(c.total(), 36.75), "{c:?}");
    }

    #[test]
    fn a_one_hour_cache_write_is_priced_at_the_one_hour_rate() {
        let t = Tokens {
            cache_write: 3_000_000,
            cache_write_1h: 1_000_000,
            ..Tokens::default()
        };
        // 2M at the 5-minute rate, 1M at the 1-hour rate.
        assert!(close(opus_4().cost(&t).cache_write, 2.0 * 6.25 + 10.0));
    }

    #[test]
    fn caching_savings_are_the_gap_between_input_and_a_cache_hit() {
        assert!(close(opus_4().caching_savings(1_000_000), 4.5));
        let sonnet = price_for("claude-sonnet-5-5").expect("priced");
        assert!(close(sonnet.caching_savings(1_000_000), 1.9));
        assert_eq!(opus_4().caching_savings(0), 0.0);
    }

    // ── fast mode and recorded speed ─────────────────────────────────────────

    #[test]
    fn every_model_with_fast_mode_has_fast_prices_and_no_other_does() {
        // The fast-mode page names three models. A fast price on any other would be invented.
        for row in PRICES {
            let has_fast =
                ["claude-opus-5-5", "claude-opus-5", "claude-opus-4-8"].contains(&row.model);
            assert_eq!(row.fast.is_some(), has_fast, "{}", row.model);
        }
    }

    #[test]
    fn a_fast_turn_is_priced_at_the_fast_mode_rates() {
        let fast = |m| rate_for(m, Some("fast"), GLOBAL).map(|r| r.prices);
        assert_eq!(
            fast("claude-opus-4-8"),
            Some(prices(10.0, 50.0, 12.5, 20.0, 1.0))
        );
        assert_eq!(
            fast("claude-opus-5"),
            Some(prices(10.0, 50.0, 12.5, 20.0, 1.0))
        );
        assert_eq!(
            fast("claude-opus-5-5"),
            Some(prices(8.0, 40.0, 10.0, 16.0, 0.4))
        );
        assert_eq!(
            fast("claude-sonnet-4-6"),
            None,
            "no fast mode, so no price for a fast turn"
        );
    }

    #[test]
    fn an_unrecorded_speed_is_a_floor_only_where_fast_mode_exists() {
        let opus = rate_for("claude-opus-4-8", None, GLOBAL).expect("priced");
        assert_eq!(opus.prices, opus_4());
        assert!(
            opus.speed_unknown,
            "fast mode costs more, so the standard price is a floor"
        );
        let sonnet = rate_for("claude-sonnet-4-6", None, GLOBAL).expect("priced");
        assert!(
            !sonnet.speed_unknown,
            "no fast mode: the standard price is the price"
        );
        let standard = rate_for("claude-opus-4-8", STD, GLOBAL).expect("priced");
        assert!(!standard.speed_unknown);
    }

    #[test]
    fn a_speed_this_build_does_not_know_has_no_price() {
        assert_eq!(rate_for("claude-opus-4-8", Some("turbo"), GLOBAL), None);
        assert_eq!(rate_for("<synthetic>", STD, GLOBAL), None);
    }

    // ── US-only inference and the recorded region ────────────────────────────

    #[test]
    fn every_model_from_4_6_on_offers_us_only_inference_and_no_older_one_does() {
        // The data residency page names the three older models in the table that reject
        // inference_geo. Every other row is 4.6 or later, at 1.1 times.
        assert_eq!(US_ONLY, 1.1);
        for row in PRICES {
            let older =
                ["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"].contains(&row.model);
            let want = if older { None } else { Some(US_ONLY) };
            assert_eq!(row.us_only, want, "{}", row.model);
        }
    }

    #[test]
    fn a_us_only_turn_costs_1_1_times_every_price_fast_modes_included() {
        let us = |m, speed| rate_for(m, speed, US).expect("priced").prices;
        let sonnet = us("claude-sonnet-4-6", STD);
        for (got, want) in [
            (sonnet.input, 3.3),
            (sonnet.output, 16.5),
            (sonnet.cache_write_5m, 4.125),
            (sonnet.cache_write_1h, 6.6),
            (sonnet.cache_read, 0.33),
        ] {
            assert!(close(got, want), "{sonnet:?}");
        }
        let fast = us("claude-opus-4-8", Some("fast"));
        assert!(
            close(fast.input, 11.0) && close(fast.output, 55.0),
            "on top of fast mode's prices: {fast:?}"
        );
        let r = rate_for("claude-sonnet-4-6", STD, US).expect("priced");
        assert!(!r.geo_unknown && !r.speed_unknown);
    }

    #[test]
    fn a_global_turn_costs_the_price() {
        let r = rate_for("claude-opus-4-8", STD, GLOBAL).expect("priced");
        assert_eq!(r.prices, opus_4());
        assert!(!r.geo_unknown);
    }

    #[test]
    fn a_region_not_known_is_a_floor_at_the_global_price_where_us_only_costs_more() {
        // No region, and the "not_available" Claude Code writes: either may have run US-only.
        for geo in [None, Some(GEO_NOT_AVAILABLE)] {
            let r = rate_for("claude-opus-4-8", STD, geo).expect("priced");
            assert_eq!(r.prices, opus_4(), "{geo:?}");
            assert!(r.geo_unknown, "{geo:?}");
            // An older model costs its price wherever it ran.
            let haiku = rate_for("claude-haiku-4-5-20251001", STD, geo).expect("priced");
            assert!(!haiku.geo_unknown, "{geo:?}");
        }
        let haiku = rate_for("claude-haiku-4-5", STD, US).expect("priced");
        assert_eq!(haiku.prices, price_for("claude-haiku-4-5").unwrap());
    }

    #[test]
    fn a_region_this_build_does_not_know_has_no_price_where_regions_are_priced() {
        assert_eq!(rate_for("claude-opus-4-8", STD, Some("eu")), None);
        assert_eq!(rate_for("claude-opus-4-8", STD, Some("US")), None);
        assert!(rate_for("claude-haiku-4-5", STD, Some("eu")).is_some());
    }

    // ── a spend across models ────────────────────────────────────────────────

    #[test]
    fn a_spend_prices_each_model_at_its_own_rate() {
        let mut s = Spend::default();
        s.add("claude-opus-4-8", STD, GLOBAL, tokens(0, 1_000_000, 0, 0));
        // Haiku 4.5 does not take inference_geo, so a turn of it that recorded no region is
        // priced in full; Sonnet 4.6 does, and recorded "global".
        s.add(
            "claude-haiku-4-5-20251001",
            None,
            None,
            tokens(0, 1_000_000, 0, 0),
        );
        s.add(
            "claude-sonnet-4-6",
            None,
            GLOBAL,
            tokens(0, 1_000_000, 0, 0),
        );
        let p = s.priced();
        assert!(close(p.cost.output, 25.0 + 5.0 + 15.0), "{p:?}");
        assert!(!p.is_partial());
        assert!(
            !p.is_floor(Figure::Total),
            "{:?}",
            p.shortfalls(Figure::Total)
        );
        assert_eq!(p.priced_tokens, 3_000_000);
    }

    #[test]
    fn a_spend_prices_a_fast_turn_at_fast_rates_beside_a_standard_one() {
        let mut s = Spend::default();
        s.add("claude-opus-4-8", STD, GLOBAL, tokens(0, 1_000_000, 0, 0));
        s.add(
            "claude-opus-4-8",
            Some("fast"),
            GLOBAL,
            tokens(0, 1_000_000, 0, 0),
        );
        assert!(close(s.priced().cost.output, 25.0 + 50.0));
    }

    #[test]
    fn a_spend_prices_a_us_only_turn_beside_a_global_one() {
        let mut s = Spend::default();
        s.add(
            "claude-opus-4-8",
            STD,
            GLOBAL,
            tokens(0, 1_000_000, 1_000_000, 0),
        );
        s.add(
            "claude-opus-4-8",
            STD,
            US,
            tokens(0, 1_000_000, 1_000_000, 0),
        );
        let p = s.priced();
        assert!(close(p.cost.output, 25.0 + 27.5), "{p:?}");
        assert!(close(p.cost.cache_read, 0.5 + 0.55), "{p:?}");
        assert!(
            close(p.caching_savings, 4.5 + 4.95),
            "a US-only cache hit saved 1.1 times as much: {p:?}"
        );
        assert!(
            !p.is_floor(Figure::Total),
            "{:?}",
            p.shortfalls(Figure::Total)
        );
    }

    #[test]
    fn a_region_not_known_makes_each_figure_in_its_scope_a_floor() {
        let mut s = Spend::default();
        s.add(
            "claude-opus-4-8",
            STD,
            Some(GEO_NOT_AVAILABLE),
            tokens(0, 100, 10, 0),
        );
        s.add("claude-haiku-4-5", STD, None, tokens(7, 0, 0, 0));
        let p = s.priced();
        assert_eq!(p.geo_unknown, tokens(0, 100, 10, 0), "not Haiku's input");
        assert_eq!(p.shortfalls(Figure::Input), vec![]);
        assert_eq!(p.shortfalls(Figure::Output), vec![Shortfall::GeoUnknown]);
        assert_eq!(
            p.shortfalls(Figure::CachingSavings),
            vec![Shortfall::GeoUnknown],
            "the saving is 1.1 times larger if the turn ran US-only"
        );
        assert_eq!(p.shortfalls(Figure::Total), vec![Shortfall::GeoUnknown]);
    }

    #[test]
    fn a_turn_in_a_region_this_build_does_not_know_is_unpriced_and_named_so() {
        let mut s = Spend::default();
        s.add(
            "claude-opus-4-8",
            Some("fast"),
            Some("eu"),
            tokens(0, 10, 0, 0),
        );
        // Empty is a value too, not an unrecorded region: the frontend names it the same way.
        s.add("claude-opus-4-8", STD, Some(""), tokens(0, 10, 0, 0));
        s.add("claude-sonnet-4-6", Some("fast"), US, tokens(0, 10, 0, 0));
        let p = s.priced();
        assert!(!p.has_figure());
        assert_eq!(
            p.unpriced_models,
            vec![
                "claude-opus-4-8 at speed fast in region eu".to_string(),
                "claude-opus-4-8 in region ".to_string(),
                "claude-sonnet-4-6 at speed fast".to_string(),
            ]
        );
    }

    #[test]
    fn an_unknown_model_adds_no_dollars_and_makes_the_total_partial() {
        let mut s = Spend::default();
        s.add("claude-opus-4-8", STD, GLOBAL, tokens(0, 1_000_000, 0, 0));
        s.add("<synthetic>", STD, GLOBAL, tokens(10, 20, 30, 40));
        s.add("", None, None, tokens(1, 0, 0, 0));
        let p = s.priced();
        assert!(
            close(p.cost.total(), 25.0),
            "unknown models add nothing: {p:?}"
        );
        assert!(p.is_partial());
        assert!(p.has_figure());
        assert_eq!(p.unpriced.total(), 101);
        assert_eq!(
            p.unpriced_models,
            vec!["".to_string(), "<synthetic>".to_string()]
        );
        assert_eq!(
            p.shortfalls(Figure::Total),
            vec![Shortfall::NoPrice(vec!["".into(), "<synthetic>".into()])]
        );
        // Their tokens are still counted.
        assert_eq!(s.tokens().cache_write, 40);
    }

    #[test]
    fn a_fast_turn_on_a_model_with_no_fast_price_is_unpriced_and_named_so() {
        let mut s = Spend::default();
        s.add(
            "claude-sonnet-4-6",
            Some("fast"),
            GLOBAL,
            tokens(0, 10, 0, 0),
        );
        let p = s.priced();
        assert!(!p.has_figure());
        assert_eq!(
            p.unpriced_models,
            vec!["claude-sonnet-4-6 at speed fast".to_string()]
        );
    }

    #[test]
    fn each_figure_is_a_floor_only_for_the_shortfalls_in_its_own_scope() {
        let mut s = Spend::default();
        // Opus output with no recorded speed, a cache write with no recorded duration on a
        // model without fast mode, and unpriced cache reads.
        s.add("claude-opus-4-8", None, GLOBAL, tokens(0, 100, 0, 0));
        s.add(
            "claude-sonnet-4-6",
            STD,
            GLOBAL,
            Tokens::of_turn(0, 0, 0, 1_000, None),
        );
        s.add("<synthetic>", STD, GLOBAL, tokens(0, 0, 50, 0));
        let p = s.priced();
        assert_eq!(p.shortfalls(Figure::Input), vec![]);
        assert_eq!(p.shortfalls(Figure::Output), vec![Shortfall::SpeedUnknown]);
        assert_eq!(
            p.shortfalls(Figure::CacheWrite),
            vec![Shortfall::DurationUnknown]
        );
        assert_eq!(
            p.shortfalls(Figure::CacheRead),
            vec![Shortfall::NoPrice(vec!["<synthetic>".into()])]
        );
        assert_eq!(
            p.shortfalls(Figure::CachingSavings),
            p.shortfalls(Figure::CacheRead),
            "the saving depends on the read and input prices, never on a write's duration"
        );
        assert_eq!(
            p.shortfalls(Figure::Total),
            vec![
                Shortfall::NoPrice(vec!["<synthetic>".into()]),
                Shortfall::SpeedUnknown,
                Shortfall::DurationUnknown,
            ]
        );
    }

    #[test]
    fn a_cache_write_with_no_recorded_duration_makes_the_cost_a_floor() {
        // The fixture turn in lib.rs: Opus 4.8, a 2,799-token cache write recorded as a 1-hour
        // write. Priced with the split it is $0.0599; without it, at the 5-minute rate,
        // $0.0494, 17.5% low, and that figure must not pass for the whole.
        let (input, output, read, write) = (3_197, 303, 16_685, 2_799);
        let mut split = Spend::default();
        split.add(
            "claude-opus-4-8",
            STD,
            GLOBAL,
            Tokens::of_turn(input, output, read, write, Some(2_799)),
        );
        let p = split.priced();
        assert!(close(p.cost.total(), 0.0598925), "{p:?}");
        assert!(!p.is_floor(Figure::Total));

        let mut unsplit = Spend::default();
        unsplit.add(
            "claude-opus-4-8",
            STD,
            GLOBAL,
            Tokens::of_turn(input, output, read, write, None),
        );
        let p = unsplit.priced();
        assert!(close(p.cost.total(), 0.04939625), "{p:?}");
        assert_eq!(p.duration_unknown, 2_799);
        assert_eq!(
            p.shortfalls(Figure::Total),
            vec![Shortfall::DurationUnknown]
        );
    }

    #[test]
    fn a_turn_with_a_recorded_split_has_no_write_of_unknown_duration() {
        let t = Tokens::of_turn(1, 2, 3, 400, Some(100));
        assert_eq!((t.cache_write_1h, t.cache_write_unknown), (100, 0));
        let t = Tokens::of_turn(1, 2, 3, 400, None);
        assert_eq!((t.cache_write_1h, t.cache_write_unknown), (0, 400));
    }

    #[test]
    fn a_spend_of_only_unknown_models_has_no_dollar_figure() {
        let mut s = Spend::default();
        s.add("mystery-model", STD, GLOBAL, tokens(100, 100, 0, 0));
        let p = s.priced();
        assert!(!p.has_figure());
        assert_eq!(p.cost.total(), 0.0);
        assert_eq!(p.caching_savings, 0.0);
    }

    #[test]
    fn an_empty_spend_is_zero_and_complete() {
        let p = Spend::default().priced();
        assert_eq!(p.cost.total(), 0.0);
        assert!(p.has_figure());
        assert!(!p.is_partial());
        assert!(!p.is_floor(Figure::Total));
    }

    #[test]
    fn a_model_seen_with_no_tokens_does_not_make_a_total_partial() {
        let mut s = Spend::default();
        s.add("<synthetic>", STD, GLOBAL, Tokens::default());
        assert!(!s.priced().is_partial());
    }

    #[test]
    fn spend_adds_up_per_model() {
        let mut s = Spend::default();
        s.add("claude-opus-4-8", STD, GLOBAL, tokens(1, 2, 3, 4));
        s.add("claude-opus-4-8", STD, GLOBAL, tokens(10, 20, 30, 40));
        s.add("claude-sonnet-4-6", None, GLOBAL, tokens(100, 0, 0, 0));
        assert_eq!(s.tokens(), tokens(111, 22, 33, 44));
    }

    #[test]
    fn every_shortfall_describes_itself() {
        assert_eq!(
            Shortfall::NoPrice(vec!["".into(), "<synthetic>".into()]).describe(),
            "no price for turns with no model, <synthetic>"
        );
        assert!(Shortfall::SpeedUnknown.describe().contains("standard rate"));
        assert_eq!(
            Shortfall::GeoUnknown.describe(),
            "inference region not known: priced at the global rate, below US-only inference's \
             1.1 times"
        );
        assert!(
            Shortfall::DurationUnknown
                .describe()
                .contains("5-minute rate")
        );
    }
}
