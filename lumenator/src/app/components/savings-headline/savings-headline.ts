import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import type { SavingsPeriod, SavingsSummary } from '../../savings-summary';
import { LumenTooltip } from '../../directives/tooltip.directive';
import {
    besideParts,
    priceBasis,
    shareLine,
    showsShare,
    tokensBand,
    tokensFigure,
    tokensSpoken,
    tokensUnit,
    usdBand,
    usdParts,
    usdSpoken,
} from '../../savings-text';

// The summary is the engine's: `lumen_savings_summary`, typed in `savings-summary.ts`. Re-exported
// so what imported it from here keeps compiling.
export type { SavingsPeriod, SavingsSummary } from '../../savings-summary';

/** One period's box, every phrase in it already laid out. */
export interface HeadlineRow {
    key: 'week' | 'allTime';
    label: string;
    /** "this week", "all time": the words the terminal ends the same phrases with. */
    when: string;
    /** Nothing recorded in the period: said in words, with no figure. */
    empty: boolean;
    /** The share line wherever the dollars are at API prices, or null. */
    share: string | null;
    /** The bold token figure, as its size, and what it is: saved, or more sent. */
    tokens: string;
    tokensUnit: string;
    tokensLoss: boolean;
    tokensSpoken: string;
    /**
     * The dollar line in three pieces, the figure the bold one: the measured net, as "at least
     * $X saved, net", "net loss of at most $X" or "net loss under half a cent". All null when
     * nothing in the period was measured.
     */
    usdLead: string | null;
    usd: string | null;
    usdTail: string | null;
    usdLoss: boolean;
    usdSpoken: string | null;
    /** "at API prices" where the user does not pay per token. */
    basis: string | null;
    /** The modelled dollars, the unpriced calls and the blocked Reads: beside the figure, not in it. */
    beside: string[];
    /** Why the period's data is partial, or null. */
    partial: string | null;
    /** The bands behind the central figures, for the tooltip. */
    band: string;
}

/**
 * The top of Home: how much Lumen saved, this week and all time, in tokens and in dollars, net,
 * both in bold.
 *
 * Everything shown is the summary's: the token figure is the central value of `tokensSaved`, and
 * the dollar figure is `measured.net` and nothing else, a floor: "at least $X saved, net", or for
 * a loss "net loss of at most $X". The modelled dollars and the calls with tokens and no dollars
 * stand beside it as tags, never in it. Wherever the dollars are at API prices (a subscription, a
 * billing mode not said or not readable) the share of tokens Lumen kept out of context comes
 * first. Each phrase is made by `savings-text.ts`, which says it as `lumen` and the tray do.
 *
 * Before a summary exists (`null`) it renders nothing at all: a neutral "nothing yet" would be
 * false for a ledger that has savings the engine has not summed, and a zero would be a figure
 * nobody measured. Once a summary records nothing, it says so in words. A summary that could not
 * be fetched is said as that (`error`): with no summary before it, in a sentence and no figure;
 * after one, its figures stay, marked as not up to date, since the last good figures with their
 * time are true, and blank ones would read as nothing saved.
 *
 * A period that cost more than it saved reads as a loss, in the alert colour.
 */
@Component({
    selector: 'lumen-savings-headline',
    imports: [RouterLink, LumenTooltip],
    templateUrl: './savings-headline.html',
    styleUrl: './savings-headline.scss',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SavingsHeadline {
    readonly summary = input<SavingsSummary | null>(null);
    /** Why the latest fetch of the summary failed; null when it did not. */
    readonly error = input<string | null>(null);

    /** Nothing recorded at all. Only said once a summary says so, never assumed from its absence. */
    readonly nothingRecorded = computed(
        () => this.summary()?.allTime.dataQuality.state === 'empty',
    );

    /** When the summary shown was made, as the engine wrote it, in UTC. */
    readonly asOf = computed(() => {
        const s = this.summary();
        return s ? s.asOf.replace('T', ' ').replace(/(\.\d+)?Z$/, ' UTC') : '';
    });

    readonly rows = computed<HeadlineRow[]>(() => {
        const s = this.summary();
        if (!s) return [];
        return [
            row(s, 'week', 'This week', 'this week', s.week),
            row(s, 'allTime', 'All time', 'all time', s.allTime),
        ];
    });
}

function row(
    s: SavingsSummary,
    key: HeadlineRow['key'],
    label: string,
    when: string,
    p: SavingsPeriod,
): HeadlineRow {
    const net = p.measured?.net ?? null;
    const parts = net === null ? null : usdParts(net.value);
    const basis = priceBasis(s.billingMode);
    const n = p.tokensSaved.value;
    const tokensBandText = `Tokens saved: band ${tokensBand(p.tokensSaved)}${s.method.calibration.calibrated
        ? '.'
        : ', in cl100k_base counts, a stand-in for Claude\'s tokenizer; the band leaves out how the two differ.'}`;
    const usdBandText = p.measured
        ? ` Dollars, net: ${usdBand(p.measured.net, p.measured.highOpen)}.`
        : '';
    return {
        key,
        label,
        when,
        empty: p.dataQuality.state === 'empty',
        share: showsShare(s.billingMode) ? shareLine(p, when) : null,
        tokens: tokensFigure(n),
        tokensUnit: tokensUnit(n),
        tokensLoss: n < 0,
        tokensSpoken: `${tokensSpoken(n)}, ${when}`,
        usdLead: parts && parts.lead !== '' ? parts.lead : null,
        usd: parts ? parts.figure : null,
        usdTail: parts && parts.tail !== '' ? parts.tail : null,
        usdLoss: net !== null && net.value < 0,
        usdSpoken: net === null ? null : `${usdSpoken(net.value)}, ${when}${basis ? `, ${basis}` : ''}`,
        basis,
        beside: besideParts(p),
        partial: p.dataQuality.state === 'partial' ? p.dataQuality.reason : null,
        band: `${label}. ${tokensBandText}${usdBandText} The figures shown are the central ones; How it's measured has every band.`,
    };
}
