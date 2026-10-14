import type { Band, BillingView, SavingsPeriod } from './savings-summary';

/**
 * How the savings figures read: `lumen_core::savings::text`, phrase for phrase.
 *
 * Bare `lumen` and the tray menu are made by that module from the same summary the headline
 * renders, so these must give the same figure, sign and label for the same value. Both are
 * tested against `savings-text.cases.json` (here by `savings-text.spec.ts` and the headline's
 * spec, there by `crates/lumen-core/tests/frontend_savings_text.rs`). Nothing here computes a
 * saving: each function lays out a number the engine sent.
 */

/** What the dollars are said to be at where the user does not pay per token. */
export const AT_API_PRICES = 'at API prices';

/** What a figure that is not a finite number reads as: never `NaN` or `Infinity`. */
export const NO_FIGURE = 'no figure';

const MINUS = '−';

/** `≈` and a no-break space: the sign and its number never wrap apart. */
const ABOUT = '≈ ';

/**
 * `x` to `places` decimals as Rust's `format!("{x:.places$}")` gives it: the exact value of the
 * double, rounded half to even. JavaScript's `toFixed` rounds a tie up (`0.125` to `0.13`, `2.5`
 * to `3`), and Rust down to the even digit (`0.12`, `2`), so a figure the engine rounds exactly
 * onto a half cent would otherwise read a cent apart in the window and the terminal.
 */
export function fixedAsRust(x: number, places: number): string {
    const abs = Math.abs(x);
    // toFixed is exact on the double's value; 100 places hold every money-sized value whole.
    if (!(abs < 1e21)) return abs.toFixed(places);
    const exact = abs.toFixed(100);
    const point = exact.indexOf('.');
    let digits = exact.slice(0, point) + exact.slice(point + 1, point + 1 + places);
    const rest = exact.slice(point + 1 + places);
    const next = rest.charCodeAt(0) - 48;
    const beyond = /[1-9]/.test(rest.slice(1));
    const odd = (digits.charCodeAt(digits.length - 1) - 48) % 2 === 1;
    if (next > 5 || (next === 5 && (beyond || odd))) digits = increment(digits);
    if (places === 0) return digits;
    return `${digits.slice(0, digits.length - places)}.${digits.slice(digits.length - places)}`;
}

/** A string of decimal digits, plus one. */
function increment(digits: string): string {
    const d = digits.split('');
    for (let i = d.length - 1; i >= 0; i--) {
        if (d[i] === '9') {
            d[i] = '0';
        } else {
            d[i] = String.fromCharCode(d[i].charCodeAt(0) + 1);
            return d.join('');
        }
    }
    return `1${d.join('')}`;
}

/** `1234567` as `1,234,567`. */
function grouped(digits: string): string {
    return digits.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

/** The digits of a dollar amount: cents below $100, whole dollars from it, grouped by thousands. */
function usdDigits(abs: number): string {
    return abs >= 100 ? grouped(fixedAsRust(abs, 0)) : fixedAsRust(abs, 2);
}

/**
 * `$1.23`, `−$0.40`, `$12,346`. The sign is the value's, never the rounding's: a loss under half
 * a cent reads `−$0.00`, and only zero reads `$0.00`. Not a number: {@link NO_FIGURE}.
 */
export function usdSigned(x: number): string {
    if (!Number.isFinite(x)) return NO_FIGURE;
    return x < 0 ? `${MINUS}$${usdDigits(-x)}` : `$${usdDigits(x)}`;
}

/** `+$1.23`, `−$0.40`: an amount beside the bold figure, its sign always shown. */
export function usdPlus(x: number): string {
    return x > 0 && Number.isFinite(x) ? `+${usdSigned(x)}` : usdSigned(x);
}

/** `12,345`, `−12,345`. */
export function tokensSigned(n: number): string {
    const digits = grouped(Math.abs(Math.trunc(n)).toString());
    return n < 0 && digits !== '0' ? `${MINUS}${digits}` : digits;
}

/** `≈ 12,345`: a token figure as its size; whether saved or added is {@link tokensUnit}'s. */
export function tokensFigure(n: number): string {
    return `${ABOUT}${grouped(Math.abs(Math.trunc(n)).toString())}`;
}

/** `tokens saved, net`, or `more tokens, net` for a period that sent more than it kept out. */
export function tokensUnit(n: number): string {
    return n < 0 ? 'more tokens, net' : 'tokens saved, net';
}

/** `≈ 12,345 tokens saved, net`, `≈ 32 more tokens, net`. */
export function tokensPhrase(n: number): string {
    return `${tokensFigure(n)} ${tokensUnit(n)}`;
}

/** The measured net in three pieces, the middle one bold. */
export interface UsdParts {
    lead: string;
    figure: string;
    tail: string;
}

/**
 * `at least` `$0.20` `saved, net`; for a loss, whose floor is the most it can be, `net loss of
 * at most` `$0.42`; for a loss that rounds to no cent, `net loss` `under half a cent`.
 */
export function usdParts(x: number): UsdParts {
    if (!Number.isFinite(x)) return { lead: '', figure: NO_FIGURE, tail: '' };
    if (x >= 0) return { lead: 'at least', figure: usdSigned(x), tail: 'saved, net' };
    const digits = usdDigits(-x);
    return digits === '0.00'
        ? { lead: 'net loss', figure: 'under half a cent', tail: '' }
        : { lead: 'net loss of at most', figure: `$${digits}`, tail: '' };
}

/** {@link usdParts} as one phrase: `at least $0.20 saved, net`, `net loss of at most $0.42`. */
export function usdPhrase(x: number): string {
    const p = usdParts(x);
    return [p.lead, p.figure, p.tail].filter((s) => s !== '').join(' ');
}

/**
 * `92% fewer` for 92% kept out of context, `5.0% more` for 5% more sent. Whole percents from
 * 10, one decimal below; only none at all is `0% fewer`.
 */
export function shareText(pct: number): string {
    if (!Number.isFinite(pct)) return NO_FIGURE;
    const abs = Math.abs(pct);
    const digits = abs >= 10 ? fixedAsRust(abs, 0) : fixedAsRust(abs, 1);
    if (pct > 0) return `${digits}% fewer`;
    if (pct < 0) return `${digits}% more`;
    return '0% fewer';
}

/** `≈ 92% fewer tokens from file and tool reads`; null with nothing read. */
export function sharePhrase(p: SavingsPeriod): string | null {
    return p.sharePct === null ? null : `${ABOUT}${shareText(p.sharePct)} tokens from file and tool reads`;
}

/** {@link sharePhrase} for `when`: `≈ 92% fewer tokens from file and tool reads this week`. */
export function shareLine(p: SavingsPeriod, when: string): string | null {
    const s = sharePhrase(p);
    return s === null ? null : `${s} ${when}`;
}

/** What the dollars are at: nothing on the API, {@link AT_API_PRICES} otherwise. */
export function priceBasis(b: BillingView): string | null {
    return b === 'api' ? null : AT_API_PRICES;
}

/** Whether the share of tokens is said first: wherever the dollars are at API prices. */
export function showsShare(b: BillingView): boolean {
    return priceBasis(b) !== null;
}

/**
 * What a period's bold figures leave out: the blocked Reads where they are all it holds, the
 * modelled dollars, and the calls with tokens and no dollars.
 */
export function besideParts(p: SavingsPeriod): string[] {
    const parts: string[] = [];
    if (p.counts.calls === 0 && p.counts.blockedReads > 0) {
        parts.push(p.counts.blockedReads === 1 ? '1 blocked Read' : `${tokensSigned(p.counts.blockedReads)} blocked Reads`);
    }
    if (p.modelled !== null) parts.push(`${usdPlus(p.modelled.value.value)} modelled`);
    if (p.unpriced.calls > 0) {
        parts.push(p.unpriced.calls === 1 ? '1 call unpriced' : `${tokensSigned(p.unpriced.calls)} calls unpriced`);
    }
    return parts;
}

// ── for a screen reader ──────────────────────────────────────────────────────────────────────
// The same figures in words: "≈" and "$" are read out as symbols, or not at all.

/**
 * `at least 105 dollars saved, net`, `net loss of at most 0.42 dollars`, `net loss under half a
 * cent`.
 */
export function usdSpoken(x: number): string {
    const p = usdParts(x);
    if (!Number.isFinite(x) || p.figure === 'under half a cent') return usdPhrase(x);
    return [p.lead, `${p.figure.slice(1)} dollars`, p.tail].filter((s) => s !== '').join(' ');
}

/** `about 12,345 tokens saved, net`, `about 32 tokens lost, net`. */
export function tokensSpoken(n: number): string {
    return `about ${tokensFigure(n).slice(ABOUT.length)} ${n < 0 ? 'tokens lost, net' : 'tokens saved, net'}`;
}

/** A dollar band, low to high, and whether its high end is a bound. */
export function usdBand(b: Band, highOpen: boolean): string {
    return `${usdSigned(b.low)} to ${usdSigned(b.high)}${highOpen ? ', and the high end is not a bound' : ''}`;
}

/** A token band, low to high. */
export function tokensBand(b: Band): string {
    return `${tokensSigned(b.low)} to ${tokensSigned(b.high)}`;
}

/** A share as `lumen report` gives it: `92.4%`, `−5.0%`. */
export function percent(pct: number): string {
    if (!Number.isFinite(pct)) return NO_FIGURE;
    return `${pct < 0 ? MINUS : ''}${fixedAsRust(pct, 1)}%`;
}
