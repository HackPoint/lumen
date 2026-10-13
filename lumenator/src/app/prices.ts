/**
 * Dollar figures, priced per model and speed from the backend's table.
 *
 * The prices come from `lumen_prices`, which serves `lumen_core::rates::PRICES`. Nothing here
 * holds a price of its own, and `crates/lumen-core/tests/frontend_prices.rs` fails if anything
 * in the frontend does: until 1.9.0 every token was priced at one model's rates, Opus-class,
 * from a constant that copied the Rust one, so a Sonnet or Haiku session was overstated.
 *
 * No figure is shown as more complete than it is. A model the table does not price adds no
 * dollars; a turn that recorded no speed, on a model whose fast mode costs more, is priced at
 * the standard rate; a cache write that recorded no duration is priced at the 5-minute rate.
 * A figure holding any of these is a floor, and {@link figureTag} says so and why. This is the
 * same reckoning as `lumen_core::rates::Priced::shortfalls`.
 */

import type { TokenAgg } from './components/index';

/** One model's list prices at one speed, in USD per million tokens. */
export interface ModelPrices {
    input: number;
    output: number;
    /** Writing to the 5-minute prompt cache. */
    cacheWrite5m: number;
    /** Writing to the 1-hour prompt cache. */
    cacheWrite1h: number;
    /** A cache hit or refresh. */
    cacheRead: number;
}

/** A row of the price table: a model's prices, or null and the reason in `note`. */
export interface PriceRow {
    provider: string;
    /** The id with no snapshot date and no context suffix, as {@link priceKey} gives it. */
    model: string;
    /** At standard speed. */
    prices: ModelPrices | null;
    /** In fast mode, for a model that has one. */
    fast: ModelPrices | null;
    note: string;
    /** Where the provider publishes its prices. */
    source: string;
}

/** The table `lumen_prices` serves. */
export interface PriceTable {
    /** The day the table was last checked against its source. */
    asOf: string;
    models: PriceRow[];
}

/** Tokens of each kind, as one model consumed them at one speed. */
export interface ModelTokens {
    /** As the transcript recorded it, dated ids included. Null or empty when it recorded none. */
    model: string | null;
    /** `usage.speed`, 'standard' or 'fast'. Null or absent when the turn did not record it. */
    speed?: string | null;
    input: number;
    output: number;
    cacheRead: number;
    /** Every cache write, whatever its duration. */
    cacheWrite: number;
    /** The part of `cacheWrite` recorded as 1-hour writes. */
    cacheWrite1h?: number;
    /**
     * The part of `cacheWrite` whose duration was not recorded, priced at the 5-minute rate.
     * Absent means not known either: every write not recorded as 1-hour is then counted here.
     */
    cacheWriteUnknown?: number;
}

/** What a turn's tokens are priced at. */
export interface Rate {
    prices: ModelPrices;
    /** The turn recorded no speed and the model has a fast mode, which costs more. */
    speedUnknown: boolean;
}

/** Token counts by kind. */
export interface Kinds {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
}

/** Tokens in dollars, by kind, and every way the dollars can fall short of the whole. */
export interface Priced {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    total: number;
    /** Cache reads valued at the input price less what they cost. Claude Code's doing, not Lumen's. */
    cacheSavings: number;
    pricedTokens: number;
    /** Tokens of a model, or a speed, with no price. In no dollar figure. */
    unpriced: Kinds;
    /** Which models those are; '' for turns that recorded none. */
    unpricedModels: string[];
    /** Tokens priced at standard rates that recorded no speed, on models with a dearer fast mode. */
    speedUnknown: Kinds;
    /** Cache writes priced at the 5-minute rate that recorded no duration. */
    durationUnknown: number;
    /** The price table had not loaded, so nothing could be priced. */
    noTable: boolean;
    /** There is a figure to show: something was priced, or there was nothing to price. */
    hasFigure: boolean;
}

/** A figure drawn from a {@link Priced}. Each can fall short for different reasons. */
export type Figure = 'total' | 'input' | 'output' | 'cacheRead' | 'cacheWrite' | 'savings';

/** What to put beside a figure that is not the whole: a short label, and why. */
export interface FigureTag {
    label: 'at least' | 'no price';
    why: string;
}

const PER_MILLION = 1_000_000;

/**
 * The id `model` is priced under: lowercased, with a context suffix such as `[1m]` and a
 * snapshot date such as `-20251001` taken off. The same rule as `lumen_core::rates::price_key`,
 * and an exact match after it, never a prefix: a prefix would price a model the table has not
 * heard of at an older model's rate.
 */
export function priceKey(model: string): string {
    let key = model.trim().toLowerCase();
    const open = key.lastIndexOf('[');
    if (key.endsWith(']') && open >= 0) key = key.slice(0, open);
    const dash = key.lastIndexOf('-');
    if (dash >= 0 && /^\d{8}$/.test(key.slice(dash + 1))) key = key.slice(0, dash);
    return key;
}

function rowFor(table: PriceTable | null, model: string | null | undefined): PriceRow | null {
    if (table === null || !model) return null;
    const key = priceKey(model);
    return table.models.find((row) => row.model === key) ?? null;
}

/** A model's prices at standard speed, or null when it has none. */
export function priceFor(table: PriceTable | null, model: string | null | undefined): ModelPrices | null {
    return rowFor(table, model)?.prices ?? null;
}

/**
 * What a turn of `model` that ran at `speed` is priced at, or null when it cannot be priced.
 * The same rule as `lumen_core::rates::rate_for`: a recorded 'fast' takes the fast-mode prices
 * and has none on a model without them; an unrecorded speed takes the standard prices, flagged
 * where fast mode exists; a speed this build does not know has no price.
 */
export function rateFor(
    table: PriceTable | null,
    model: string | null | undefined,
    speed: string | null | undefined,
): Rate | null {
    const row = rowFor(table, model);
    if (row === null) return null;
    let prices: ModelPrices | null;
    let speedUnknown = false;
    if (speed === 'standard') {
        prices = row.prices;
    } else if (speed === 'fast') {
        prices = row.fast;
    } else if (speed === null || speed === undefined) {
        prices = row.prices;
        speedUnknown = row.fast !== null;
    } else {
        prices = null;
    }
    return prices === null ? null : { prices, speedUnknown };
}

function usd(tokens: number, perMillion: number): number {
    return (tokens * perMillion) / PER_MILLION;
}

function noKinds(): Kinds {
    return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
}

function addKinds(into: Kinds, row: ModelTokens): void {
    into.input += row.input;
    into.output += row.output;
    into.cacheRead += row.cacheRead;
    into.cacheWrite += row.cacheWrite;
}

function kindsTotal(k: Kinds): number {
    return k.input + k.output + k.cacheRead + k.cacheWrite;
}

/**
 * Each row priced at its own model's rates and speed.
 *
 * With no table yet (`null`) nothing is priced, so a figure shows as absent until the prices
 * arrive rather than as zero.
 */
export function price(rows: readonly ModelTokens[], table: PriceTable | null): Priced {
    const out: Priced = {
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        total: 0,
        cacheSavings: 0,
        pricedTokens: 0,
        unpriced: noKinds(),
        unpricedModels: [],
        speedUnknown: noKinds(),
        durationUnknown: 0,
        noTable: table === null,
        hasFigure: true,
    };
    for (const row of rows) {
        const tokens = row.input + row.output + row.cacheRead + row.cacheWrite;
        const rate = rateFor(table, row.model, row.speed);
        if (rate === null) {
            if (tokens !== 0) {
                addKinds(out.unpriced, row);
                const model = row.model ?? '';
                const name = row.speed && row.speed !== 'standard' ? `${model} at speed ${row.speed}` : model;
                if (!out.unpricedModels.includes(name)) out.unpricedModels.push(name);
            }
            continue;
        }
        const p = rate.prices;
        const oneHour = row.cacheWrite1h ?? 0;
        out.input += usd(row.input, p.input);
        out.output += usd(row.output, p.output);
        out.cacheRead += usd(row.cacheRead, p.cacheRead);
        out.cacheWrite += usd(row.cacheWrite - oneHour, p.cacheWrite5m) + usd(oneHour, p.cacheWrite1h);
        out.cacheSavings += usd(row.cacheRead, p.input - p.cacheRead);
        out.pricedTokens += tokens;
        out.durationUnknown += row.cacheWriteUnknown ?? row.cacheWrite - oneHour;
        if (rate.speedUnknown) addKinds(out.speedUnknown, row);
    }
    out.total = out.input + out.output + out.cacheRead + out.cacheWrite;
    out.hasFigure = out.pricedTokens > 0 || kindsTotal(out.unpriced) === 0;
    return out;
}

/** The figure's dollars. */
export function amount(p: Priced, figure: Figure): number {
    return figure === 'savings' ? p.cacheSavings : p[figure];
}

/** Of `k`, the tokens `figure` is made of. */
function inScope(k: Kinds, figure: Figure): number {
    switch (figure) {
        case 'total': return kindsTotal(k);
        case 'savings': return k.cacheRead;
        default: return k[figure];
    }
}

/** Every reason `figure` can be lower than the truth, one sentence each. Empty when exact. */
export function shortfalls(p: Priced, figure: Figure): string[] {
    const out: string[] = [];
    if (inScope(p.unpriced, figure) > 0) {
        if (p.noTable) {
            out.push('Prices have not loaded from Lumen yet, so nothing here is priced.');
        } else {
            const names = p.unpricedModels.map((m) => m || 'turns with no model');
            out.push(`No price for ${names.join(', ')}.`);
        }
    }
    if (inScope(p.speedUnknown, figure) > 0) {
        out.push('Some turns did not record their speed and are priced at the standard rate; '
            + 'fast mode costs more.');
    }
    if (p.durationUnknown > 0 && (figure === 'total' || figure === 'cacheWrite')) {
        out.push('Some cache writes did not record how long they were cached (turns before 1.9.0) '
            + 'and are priced at the 5-minute rate, the lower one.');
    }
    return out;
}

/** `figure` is a floor: what was really spent can be higher. */
export function isFloor(p: Priced, figure: Figure): boolean {
    return shortfalls(p, figure).length > 0;
}

/**
 * The tag a figure carries when it is not the whole: 'no price' when nothing behind it could be
 * priced and it shows as a dash, 'at least' when it is a floor, null when it is exact.
 */
export function figureTag(p: Priced, figure: Figure): FigureTag | null {
    const why = shortfalls(p, figure);
    if (why.length === 0) return null;
    return { label: p.hasFigure ? 'at least' : 'no price', why: why.join(' ') };
}

/**
 * An aggregate's tokens by model and speed. A backend older than 1.9.0 sends no split, and its
 * sums then go under no model, which has no price.
 */
export function aggTokens(agg: TokenAgg): ModelTokens[] {
    return agg.byModel ?? [{
        model: null,
        input: agg.input,
        output: agg.output,
        cacheRead: agg.cacheRead,
        cacheWrite: agg.cacheWrite,
    }];
}

/** A figure in dollars, or a dash when none of the tokens behind it has a price. */
export function fmtUsd(p: Priced, figure: Figure): string {
    return p.hasFigure ? '$' + amount(p, figure).toFixed(2) : '—';
}
