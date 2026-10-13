import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

/**
 * One period of the savings summary the headline shows.
 *
 * The shape the savings engine is to fill. Nothing produces it yet, and nothing here computes
 * one: the headline only lays out what it is given.
 */
export interface SavingsPeriod {
    /** Tokens Lumen kept out of Claude Code's context, net of any it added. Signed: a loss is negative. */
    tokens: number;
    /** The same in dollars, net and signed. Null when it cannot be priced, which is said, never shown as $0. */
    usd: number | null;
    /** True when `usd` rests on a model rather than a measurement; the figure is labelled so. */
    usdModelled: boolean;
    /** How many recorded events the figures cover. 0 means nothing was recorded in the period. */
    events: number;
}

/** What the headline shows: this week (local, from Monday) and all time. */
export interface SavingsSummary {
    week: SavingsPeriod;
    allTime: SavingsPeriod;
}

/**
 * The top of Home: how much Lumen saved, this week and all time, in tokens and in dollars, net.
 *
 * Before a summary exists (`null`) it renders nothing at all. A neutral "nothing yet" would be
 * false for a ledger that has savings the engine has not summed, and a zero would be a figure
 * nobody measured. Once a summary exists and records no event, it says so in words.
 *
 * Both figures are signed and shown as they are: a period that cost more than it saved reads as
 * a loss, never as zero.
 */
@Component({
    selector: 'lumen-savings-headline',
    templateUrl: './savings-headline.html',
    styleUrl: './savings-headline.scss',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SavingsHeadline {
    readonly summary = input<SavingsSummary | null>(null);

    /** Nothing recorded at all. Only said once a summary says so, never assumed from its absence. */
    readonly nothingRecorded = computed(() => this.summary()?.allTime.events === 0);

    readonly periods = computed(() => {
        const s = this.summary();
        return s
            ? [
                  { label: 'This week', period: s.week },
                  { label: 'All time', period: s.allTime },
              ]
            : [];
    });

    /** `+1,234`, `−1,234` or `0`: the sign is part of the figure. */
    fmtTokens(n: number): string {
        const abs = Math.abs(Math.round(n)).toLocaleString('en-US');
        return abs === '0' ? '0' : `${n < 0 ? '−' : '+'}${abs}`;
    }

    /**
     * `+$1.23`, `−$1.23`, or `—` when it could not be priced. Whole dollars from $100, as the
     * Optimizer's figure was. A value that rounds to nothing carries no sign, so it cannot read as
     * a gain or a loss that did not happen.
     */
    fmtUsd(n: number | null): string {
        if (n === null) return '—';
        const abs = Math.abs(n);
        const digits = abs >= 100 ? abs.toFixed(0) : abs.toFixed(2);
        return Number(digits) === 0 ? '$0.00' : `${n < 0 ? '−' : '+'}$${digits}`;
    }
}
