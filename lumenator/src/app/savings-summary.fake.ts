import type {
    Band,
    BillingView,
    CallLine,
    DataQuality,
    Dollars,
    Modelled,
    Recommendation,
    SavingsCounts,
    SavingsPeriod,
    SavingsStatus,
    SavingsSummary,
    Unpriced,
} from './savings-summary';

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

/** A period as `savings-text.cases.json` names it: only what the text reads. */
export interface CasePeriod {
    tokensSaved?: number;
    sharePct?: number | null;
    net?: number | null;
    /** The value of the tokens before the extra rounds; the net unless the case says. */
    gross?: number;
    extraRounds?: number;
    modelled?: number | null;
    unpricedCalls?: number;
    calls?: number;
    blockedReads?: number;
    dataQuality: DataQuality;
}

/** A summary as `savings-text.cases.json` names it. */
export interface CaseSummary {
    billingMode: BillingView;
    status: SavingsStatus;
    recommendation?: Recommendation | null;
    week: CasePeriod;
    allTime: CasePeriod;
}

function casePeriod(c: CasePeriod): SavingsPeriod {
    return savingsPeriod({
        tokensSaved: band(c.tokensSaved ?? 0),
        sharePct: c.sharePct ?? null,
        measured: c.net === undefined || c.net === null
            ? null
            : dollars(c.net, { gross: band(c.gross ?? c.net), extraRounds: band(c.extraRounds ?? 0) }),
        modelled: c.modelled === undefined || c.modelled === null ? null : modelled(c.modelled),
        unpriced: unpriced({ calls: c.unpricedCalls ?? 0 }),
        counts: savingsCounts({ calls: c.calls ?? 0, blockedReads: c.blockedReads ?? 0 }),
        dataQuality: c.dataQuality,
    });
}

/** The full summary a case names, the rest as an empty summary has it, as the Rust test builds it. */
export function caseSummary(c: CaseSummary): SavingsSummary {
    return savingsSummary({
        billingMode: c.billingMode,
        status: c.status,
        recommendation: c.recommendation ?? null,
        week: casePeriod(c.week),
        allTime: casePeriod(c.allTime),
    });
}

/** A call line read by a turn of its own, priced, unless told otherwise. */
export function callLine(over: Partial<CallLine> = {}): CallLine {
    return {
        ts: '2026-10-14T11:00:00Z',
        kind: 'call',
        tool: 'smart_read',
        route: 'smart_read',
        path: '/repo/src/lib.rs',
        inWeek: true,
        savedCl100k: 9_000,
        recordedSavedCl100k: 9_000,
        savedAgainst: 'wholeFile',
        fullCl100k: 10_000,
        readCapped: false,
        tokensSaved: band(9_000),
        provenance: 'measured',
        pair: {
            basis: 'derived',
            model: 'claude-opus-4-8',
            speed: 'standard',
            region: 'global',
            rounds: 12,
            roundsBound: 'sessionEnd',
            roundsTrusted: true,
            ttl: '5m',
            someRoundsUnpriced: false,
        },
        extraRounds: 0,
        extraRoundBasis: 'blockNotRecorded',
        extraRoundEvidenced: false,
        dollars: 'measured',
        modelledValueUsd: null,
        valueUsd: band(0.11),
        extraRoundUsd: band(0),
        netUsd: band(0.11),
        pricingUnknown: [],
        ...over,
    };
}
