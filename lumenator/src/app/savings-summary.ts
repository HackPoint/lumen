/**
 * What Lumen saved: the wire format of `lumen_savings_summary`, which is
 * `lumen_core::savings::SavingsSummary` serialized. The CLI's `lumen report --json` prints the
 * same document from the same engine.
 *
 * The frontend renders this and computes nothing: every figure, sign, band, label and count is
 * the engine's. `crates/lumen-core/tests/frontend_savings.rs` fails when a field the engine sends
 * is missing here.
 */

/** A low, central and high estimate. Central is the conservative figure. */
export interface Band {
    low: number;
    value: number;
    high: number;
}

export type BillingView = 'subscription' | 'api' | 'unset' | 'unreadable';

/** One part of a period's dollars. */
export interface Dollars {
    /** Calls and blocked Reads in it. */
    rows: number;
    gross: Band;
    extraRounds: Band;
    /** `gross` less `extraRounds`, signed. */
    net: Band;
    /** `net.high` is no upper bound. */
    highOpen: boolean;
}

/**
 * What calls whose R may be too long are worth beyond their worth at R = 0, which is in the
 * measured figure: the cache reads over that R.
 */
export interface Modelled {
    /** Calls it comes from. */
    rows: number;
    value: Band;
}

/** Rows with tokens and no dollars. */
export interface Unpriced {
    calls: number;
    blockedReads: number;
    /** The calls' tokens saved, also counted in the period's `tokensSaved`. */
    tokens: Band;
    /** Rows with no session recorded. */
    noSession: number;
    /** Rows with a session and no turn in the ledger that read them. */
    noTurn: number;
    /** Rows whose consuming model has no price. */
    noPrice: number;
}

export interface SavingsCounts {
    /** Lumen calls. */
    calls: number;
    /** Reads the intercept blocked and its escape valve then let through: a round for nothing. */
    blockedReads: number;
    /** Calls whose R and C came from their own turn chain. */
    derivedPairs: number;
    unverifiedProvenanceRows: number;
    extraRoundsEvidenced: number;
    extraRoundsModelled: number;
    /** Derived pairs whose R is a lower bound: the session may still be running. */
    roundsLowerBoundRows: number;
    /**
     * Derived pairs whose R may be too long: a conversation from before the daemon recorded
     * compactions, with no recorded event to end it.
     */
    roundsUnrecordedRows: number;
    /** Derived pairs whose R a collapse ended: inferred from the prompt shrinking, not recorded. */
    roundsCollapseRows: number;
    /** The escape-valve faults behind the blocked Reads: one on every later Read of a blocked path. */
    valveFaults: number;
    /**
     * Blocked Reads counted because faults no evidence attributes could stand for them: one per
     * agent that could have read the path then, beyond the first. Modelled, at the most.
     */
    blockedReadsBounded: number;
    unreadableTimeRows: number;
}

export type DataQuality =
    | { state: 'complete' }
    | { state: 'partial'; reason: string }
    | { state: 'empty' };

export interface SavingsPeriod {
    /** ISO-8601 UTC: the week's local Monday 00:00, or the first entry of all time. */
    from: string | null;
    to: string;
    /** Claude tokens kept out of context, net of any added. Signed. */
    tokensSaved: Band;
    /** Claude tokens the calls' files held. */
    tokensFull: Band;
    /** tokensSaved / tokensFull at the central figures, percent; null with nothing read. */
    sharePct: number | null;
    /**
     * The figure shown in bold, as "at least": dollars measured from the calls' own turns, a
     * floor. Null when nothing in the period could be measured: never shown as $0.
     */
    measured: Dollars | null;
    /**
     * What calls whose R may be too long are worth beyond R = 0. Shown beside `measured`,
     * labelled modelled, never added to it.
     */
    modelled: Modelled | null;
    /** Rows with tokens and no dollars: no consuming turn to price them by, or no price. */
    unpriced: Unpriced;
    /** What the turns behind the dollars did not record, in the app's own words. */
    usdShortfalls: string[];
    counts: SavingsCounts;
    dataQuality: DataQuality;
}

export type SavingsStatus =
    | { state: 'working' }
    | { state: 'notWorking'; reason: string }
    | { state: 'noData' };

export interface Recommendation {
    code:
        | 'blocksWithoutSavings'
        | 'noCalls'
        | 'identityHook'
        | 'unplacedCalls'
        | 'compactsEarly'
        | 'smallSavings';
    text: string;
}

export interface CalibrationInfo {
    /** False while token figures are cl100k_base counts, a proxy for Claude's. */
    calibrated: boolean;
    asOf: string | null;
    note: string;
}

export interface SavingsMethod {
    pricesAsOf: string;
    calibration: CalibrationInfo;
    week: string;
    /** What a saving is counted against: the built-in Read, capped at what one returns. */
    counterfactual: string;
    rounds: string;
    roundCost: string;
    extraRounds: string;
    /** How a turn that recorded no speed, region or cache duration is priced. */
    floors: string;
    /** What goes in the measured, the modelled and the tokens-only parts. */
    parts: string;
}

export interface PairView {
    basis:
        | 'derived'
        | 'noSession'
        | 'unreadableTime'
        | 'noFollowingTurn'
        | 'clearedBeforeRead'
        | 'subagentNotInLedger';
    model: string | null;
    speed: string | null;
    /** The consuming turn's `inference_geo`. */
    region: string | null;
    rounds: number | null;
    /**
     * What ended R: a recorded `compaction` or `rewind`; a `collapse` (inferred: a later prompt
     * smaller than the one before it, so context was taken out); the session's end, in a transcript the daemon
     * recorded events for (`sessionEnd`) or from before it did (`sessionEndUnrecorded`: R may be
     * too long); or `sessionOpen` (it may still be running: R is a lower bound).
     */
    roundsBound:
        | 'compaction'
        | 'rewind'
        | 'collapse'
        | 'sessionEnd'
        | 'sessionEndUnrecorded'
        | 'sessionOpen'
        | null;
    /** R cannot be too long. False where an unrecorded compaction could have ended it sooner. */
    roundsTrusted: boolean;
    ttl: '1h' | '5m' | 'mixed' | 'unrecorded' | 'uncached' | null;
    someRoundsUnpriced: boolean;
}

export interface CallLine {
    ts: string;
    kind: 'call' | 'blockedRead';
    tool: 'smart_read' | 'recall_file' | 'compress_logs' | null;
    route: string | null;
    path: string | null;
    inWeek: boolean;
    /** What is counted as saved, in cl100k_base tokens, against `savedAgainst`. */
    savedCl100k: number;
    /** What the ledger recorded, against the whole file. */
    recordedSavedCl100k: number;
    /**
     * `wholeFile` (a Read, at most what one returns); `previousAnswer` for a repeat read, against
     * the answer Lumen would have re-sent; `answerNotInLedger` (counted as none);
     * `alreadyInContext` for inline text; `askedAgain` for a fresh ask after a stub.
     */
    savedAgainst: 'wholeFile' | 'previousAnswer' | 'answerNotInLedger' | 'alreadyInContext' | 'askedAgain';
    fullCl100k: number;
    /** The file is larger than a Read returns, so it counts as what one returns. */
    readCapped: boolean;
    tokensSaved: Band;
    provenance: 'measured' | 'unverified';
    pair: PairView;
    extraRounds: number;
    extraRoundBasis:
        | 'repeatAnswer'
        | 'followUp'
        | 'freshAfterStub'
        | 'inlineText'
        | 'blockNotRecorded'
        | 'blockedRead'
        | 'blockedReadBounded';
    extraRoundEvidenced: boolean;
    /** `measured` when the line has dollars (all in the measured figure); `none`: tokens only. */
    dollars: 'measured' | 'none';
    /** What the line is worth beyond R = 0 where R may be too long: modelled, not in `netUsd`. */
    modelledValueUsd: Band | null;
    valueUsd: Band | null;
    extraRoundUsd: Band | null;
    netUsd: Band | null;
    /** What the turns pricing it did not record: `speed`, `region`, `cacheDuration`. */
    pricingUnknown: Array<'speed' | 'region' | 'cacheDuration'>;
}

export interface CallLedger {
    /** Newest first, at most 200. */
    recent: CallLine[];
    omitted: number;
    topGains: CallLine[];
    topLosses: CallLine[];
}

export interface SavingsSummary {
    schema: number;
    asOf: string;
    billingMode: BillingView;
    /** The ISO week, Monday 00:00 local time to now. */
    week: SavingsPeriod;
    allTime: SavingsPeriod;
    status: SavingsStatus;
    recommendation: Recommendation | null;
    method: SavingsMethod;
    ledger: CallLedger;
}
