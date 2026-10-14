import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { Firefly } from '../../components/firefly/firefly';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SessionService } from '../../session.service';
import type { Band, CallLine, Dollars, SavingsPeriod, SavingsSummary } from '../../savings-summary';
import type { ModelPrices, PriceRow } from '../../prices';
import { percent, priceBasis, tokensSigned, usdBand, usdPlus, usdSigned } from '../../savings-text';

/** A row of the figures-and-counts table: what it is, then this week's and all time's. */
export interface FigureRow {
    what: string;
    week: string;
    allTime: string;
}

/** A row of the price table. */
export interface PriceLine {
    model: string;
    cells: string[] | null;
    note: string;
}

/** A call line as the ledger table shows it. */
export interface LedgerRow {
    ts: string;
    what: string;
    path: string;
    tokens: string;
    against: string;
    net: string;
    readBy: string;
    extraRound: string;
    loss: boolean;
}

/** What each `savedAgainst` counts a saving against, in words. */
const AGAINST: Record<CallLine['savedAgainst'], string> = {
    wholeFile: 'the file, as a Read returns it',
    previousAnswer: 'the earlier answer',
    answerNotInLedger: 'an earlier answer not in the ledger (none)',
    alreadyInContext: 'text already in context (none)',
    askedAgain: 'asked again after a stub (none)',
};

/** What ended a call's R, in the words `lumen report` uses. */
const BOUND: Record<NonNullable<CallLine['pair']['roundsBound']>, string> = {
    compaction: 'to a compaction',
    rewind: 'to a rewind',
    collapse: 'to a collapse in the prompt (inferred)',
    sessionOpen: 'so far, the session may still be running',
    sessionEndUnrecorded: "to the session's end, unrecorded (may be too long)",
    sessionEnd: "to the session's end",
};

const EXTRA: Record<CallLine['extraRoundBasis'], string> = {
    repeatAnswer: 'a repeat answer',
    followUp: 'a follow-up after an outline',
    freshAfterStub: 'a fresh ask after a stub',
    inlineText: 'writing inline text out',
    blockNotRecorded: 'a blocked Read that may have come first',
    blockRecorded: 'the recorded block before it, counted on its own line',
    blockedRead: 'the blocked Read',
    blockedReadBounded: 'a blocked Read no evidence attributes',
    blockedReadRecorded: 'the blocked Read, as its transcript records it',
};

/**
 * How it's measured: everything behind Home's headline, from the same summary.
 *
 * The rules each figure is made by (the summary's own text), the price table and its date,
 * whether token counts are calibrated, every count and band for each period, and the per-call
 * ledger (the newest 200 calls and the five that gained and lost most) as tables. Reached from
 * the headline and from Details. Computes nothing: each cell is a value the engine or the price
 * table sent, laid out by `savings-text.ts` as the terminal lays it out.
 */
@Component({
    selector: 'lumen-method',
    imports: [Firefly, TabNav],
    templateUrl: './method.html',
    styleUrl: './method.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Method {
    readonly s = inject(SessionService);

    readonly summary = this.s.savingsSummary;
    readonly error = this.s.savingsError;

    readonly rules = computed(() => {
        const m = this.summary()?.method;
        if (!m) return [];
        return [
            { term: 'This week', text: m.week },
            { term: 'What a saving is counted against', text: m.counterfactual },
            { term: 'R, the rounds a saved token stays out', text: m.rounds },
            { term: 'C, the cost of one extra round', text: m.roundCost },
            { term: 'Extra rounds', text: m.extraRounds },
            { term: 'Prices a turn did not record', text: m.floors },
            { term: 'Measured, modelled and tokens only', text: m.parts },
        ];
    });

    /**
     * Whether token counts are calibrated, said once: the engine's own note, which says it is
     * uncalibrated where it is, after the date it was calibrated on where it is.
     */
    readonly calibration = computed<string[]>(() => {
        const c = this.summary()?.method.calibration;
        if (!c) return [];
        const head = c.calibrated ? [`Calibrated${c.asOf ? ` as of ${c.asOf}` : ''}.`] : [];
        const note = c.note || "Uncalibrated: counts are cl100k_base, a stand-in for Claude's tokenizer.";
        return [...head, note];
    });

    readonly basis = computed(() => {
        const s = this.summary();
        return s ? priceBasis(s.billingMode) : null;
    });

    /** Where the price table comes from, each source once. */
    readonly priceSources = computed(() => [...new Set((this.s.prices()?.models ?? []).map((m) => m.source))]);

    readonly priceLines = computed<PriceLine[]>(() => {
        const out: PriceLine[] = [];
        for (const row of this.s.prices()?.models ?? []) {
            out.push(priceLine(row.model, row.prices, row));
            if (row.fast) out.push(priceLine(`${row.model}, fast mode`, row.fast, row));
        }
        return out;
    });

    readonly figures = computed<FigureRow[]>(() => {
        const s = this.summary();
        return s ? figureRows(s) : [];
    });

    readonly shortfalls = computed(() => {
        const s = this.summary();
        return s ? [...new Set([...s.week.usdShortfalls, ...s.allTime.usdShortfalls])] : [];
    });

    readonly gains = computed(() => (this.summary()?.ledger.topGains ?? []).map(ledgerRow));
    readonly losses = computed(() => (this.summary()?.ledger.topLosses ?? []).map(ledgerRow));
    readonly recent = computed(() => (this.summary()?.ledger.recent ?? []).map(ledgerRow));
    readonly omitted = computed(() => this.summary()?.ledger.omitted ?? 0);

    readonly tables = computed(() => [
        { id: 'gains', title: 'Gained most, all time', rows: this.gains() },
        { id: 'losses', title: 'Lost most, all time', rows: this.losses() },
        { id: 'recent', title: 'Newest first', rows: this.recent() },
    ]);

    readonly asOf = computed(() => when(this.summary()?.asOf ?? ''));
}

/** An ISO time as the engine wrote it, in UTC. */
function when(iso: string): string {
    return iso.replace('T', ' ').replace(/(\.\d+)?Z$/, ' UTC');
}

/** A price per million tokens, to the digit the table gives it. */
function price(n: number): string {
    return `$${n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 6 })}`;
}

function priceLine(model: string, p: ModelPrices | null, row: PriceRow): PriceLine {
    return {
        model,
        cells: p ? [p.input, p.output, p.cacheWrite5m, p.cacheWrite1h, p.cacheRead].map(price) : null,
        note: p ? (row.usOnly !== null ? `US-only inference: ${row.usOnly} times` : '') : row.note || 'No price.',
    };
}

function calls(n: number): string {
    return n === 1 ? '1 call' : `${tokensSigned(n)} calls`;
}

function tokenBand(b: Band): string {
    return b.low === b.high ? tokensSigned(b.value) : `${tokensSigned(b.value)} (band ${tokensSigned(b.low)} to ${tokensSigned(b.high)})`;
}

function dollarsBand(d: Dollars | null, pick: (d: Dollars) => Band, open: boolean): string {
    if (!d) return 'none measured';
    const b = pick(d);
    return `${usdSigned(b.value)} (band ${usdBand(b, open && d.highOpen)})`;
}

function quality(p: SavingsPeriod): string {
    switch (p.dataQuality.state) {
        case 'complete':
            return 'complete';
        case 'empty':
            return 'nothing recorded';
        case 'partial':
            return `partial: ${p.dataQuality.reason}`;
    }
}

function figureRows(s: SavingsSummary): FigureRow[] {
    const both = (what: string, f: (p: SavingsPeriod) => string): FigureRow => ({ what, week: f(s.week), allTime: f(s.allTime) });
    const n = (v: number) => tokensSigned(v);
    return [
        both('Tokens saved, net', (p) => tokenBand(p.tokensSaved)),
        both('Tokens the files held', (p) => tokenBand(p.tokensFull)),
        both('Share kept out of context', (p) => (p.sharePct === null ? 'nothing read' : percent(p.sharePct))),
        both('Measured value of the tokens', (p) => dollarsBand(p.measured, (d) => d.gross, false)),
        both('Measured extra rounds', (p) => dollarsBand(p.measured, (d) => d.extraRounds, false)),
        both('Measured net, at least (the bold figure)', (p) => dollarsBand(p.measured, (d) => d.net, true)),
        both('Rows measured', (p) => n(p.measured?.rows ?? 0)),
        both('Modelled, not added', (p) => (p.modelled ? `${usdPlus(p.modelled.value.value)} from ${calls(p.modelled.rows)} (band ${usdPlus(p.modelled.value.low)} to ${usdPlus(p.modelled.value.high)})` : 'none')),
        both('Tokens only: calls', (p) => n(p.unpriced.calls)),
        both('Tokens only: blocked Reads', (p) => n(p.unpriced.blockedReads)),
        both('Tokens only: tokens', (p) => tokenBand(p.unpriced.tokens)),
        both('Tokens only: no session recorded', (p) => n(p.unpriced.noSession)),
        both('Tokens only: no turn that read it', (p) => n(p.unpriced.noTurn)),
        both('Tokens only: a model with no price', (p) => n(p.unpriced.noPrice)),
        both('Lumen calls', (p) => n(p.counts.calls)),
        both('Calls with their own R and C', (p) => n(p.counts.derivedPairs)),
        both('Blocked Reads', (p) => n(p.counts.blockedReads)),
        both('of them recorded in the transcript of the agent that tried', (p) => n(p.counts.blockedReadsFromTranscripts)),
        both('of them from escape-valve faults, on evidence', (p) => n(p.counts.blockedReadsFromFaults)),
        both('of them bounded, where no evidence says whose (modelled, at the most)', (p) => n(p.counts.blockedReadsBounded)),
        both('Escape-valve faults behind them', (p) => n(p.counts.valveFaults)),
        both('Extra rounds on evidence', (p) => n(p.counts.extraRoundsEvidenced)),
        both('Extra rounds modelled', (p) => n(p.counts.extraRoundsModelled)),
        both('R a lower bound (session may be running)', (p) => n(p.counts.roundsLowerBoundRows)),
        both('R from before compactions were recorded', (p) => n(p.counts.roundsUnrecordedRows)),
        both('R ended by a collapse (inferred)', (p) => n(p.counts.roundsCollapseRows)),
        both('Token counts not known to be measured', (p) => n(p.counts.unverifiedProvenanceRows)),
        both('Times that could not be read', (p) => n(p.counts.unreadableTimeRows)),
        both('Data', quality),
    ];
}

function ledgerRow(l: CallLine): LedgerRow {
    const what = l.kind !== 'blockedRead'
        ? (l.tool ?? '?')
        : l.extraRoundBasis === 'blockedReadRecorded'
          ? 'blocked Read, from its transcript'
          : 'blocked Read, then let through';
    const p = l.pair;
    const readBy = p.model
        ? `${p.model}${p.rounds !== null && p.roundsBound ? `, R ${p.rounds} ${BOUND[p.roundsBound]}` : ''}`
        : `no dollars: ${p.basis}`;
    const net = l.netUsd
        ? `${usdSigned(l.netUsd.value)}${l.modelledValueUsd ? ` (${usdPlus(l.modelledValueUsd.value)} modelled)` : ''}`
        : 'tokens only';
    return {
        ts: when(l.ts),
        what,
        path: l.path ?? '',
        tokens: `${tokensSigned(l.tokensSaved.value)}${l.readCapped ? ', capped at what a Read returns' : ''}`,
        against: AGAINST[l.savedAgainst],
        net,
        readBy,
        extraRound: l.extraRounds === 0
            ? l.extraRoundBasis === 'blockRecorded' ? `none: ${EXTRA.blockRecorded}` : 'none'
            : `${l.extraRounds}: ${EXTRA[l.extraRoundBasis]}, ${l.extraRoundEvidenced ? 'on evidence' : 'modelled'}`,
        loss: (l.netUsd?.value ?? l.tokensSaved.value) < 0,
    };
}
