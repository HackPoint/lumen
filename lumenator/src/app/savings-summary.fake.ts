import type { Band, Dollars, Modelled, SavingsCounts, SavingsPeriod, SavingsSummary, Unpriced } from './savings-summary';

/**
 * Summaries in the engine's shape, for specs. Test data only: the app receives every figure from
 * `lumen_savings_summary` and builds none of its own.
 */

/** A band with no width. */
export function band(value: number): Band {
    return { low: value, value, high: value };
}

/** A part of a period's dollars with `net` and no width. */
export function dollars(net: number, over: Partial<Dollars> = {}): Dollars {
    return { rows: 1, gross: band(net), extraRounds: band(0), net: band(net), highOpen: false, ...over };
}

/** A modelled amount with no width. */
export function modelled(value: number, rows = 1): Modelled {
    return { rows, value: band(value) };
}

export function unpriced(over: Partial<Unpriced> = {}): Unpriced {
    return { calls: 0, blockedReads: 0, tokens: band(0), noSession: 0, noTurn: 0, noPrice: 0, ...over };
}

export function savingsCounts(over: Partial<SavingsCounts> = {}): SavingsCounts {
    return {
        calls: 0,
        blockedReads: 0,
        derivedPairs: 0,
        unverifiedProvenanceRows: 0,
        extraRoundsEvidenced: 0,
        extraRoundsModelled: 0,
        roundsLowerBoundRows: 0,
        roundsUnrecordedRows: 0,
        roundsCollapseRows: 0,
        valveFaults: 0,
        blockedReadsBounded: 0,
        unreadableTimeRows: 0,
        ...over,
    };
}

/** An empty period unless told otherwise. */
export function savingsPeriod(over: Partial<SavingsPeriod> = {}): SavingsPeriod {
    return {
        from: null,
        to: '2026-10-14T12:00:00Z',
        tokensSaved: band(0),
        tokensFull: band(0),
        sharePct: null,
        measured: null,
        modelled: null,
        unpriced: unpriced(),
        usdShortfalls: [],
        counts: savingsCounts(),
        dataQuality: { state: 'empty' },
        ...over,
    };
}

/** A period that recorded something: `tokens` saved, `usd` measured net (null: none measured). */
export function recordedPeriod(tokens: number, usd: number | null, over: Partial<SavingsPeriod> = {}): SavingsPeriod {
    return savingsPeriod({
        tokensSaved: band(tokens),
        measured: usd === null ? null : dollars(usd),
        counts: savingsCounts({ calls: 1 }),
        dataQuality: { state: 'complete' },
        ...over,
    });
}

export function savingsSummary(over: Partial<SavingsSummary> = {}): SavingsSummary {
    return {
        schema: 1,
        asOf: '2026-10-14T12:00:00Z',
        billingMode: 'api',
        week: savingsPeriod(),
        allTime: savingsPeriod(),
        status: { state: 'noData' },
        recommendation: null,
        method: {
            pricesAsOf: '2026-10-13',
            calibration: { calibrated: false, asOf: null, note: '' },
            week: '',
            counterfactual: '',
            rounds: '',
            roundCost: '',
            extraRounds: '',
            floors: '',
            parts: '',
        },
        ledger: { recent: [], omitted: 0, topGains: [], topLosses: [] },
        ...over,
    };
}
