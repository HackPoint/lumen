import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import type { SavingsSummary } from '../../savings-summary';

// The summary is the engine's: `lumen_savings_summary`, typed in `savings-summary.ts`. Re-exported
// so what imported it from here keeps compiling.
export type { SavingsPeriod, SavingsSummary } from '../../savings-summary';

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
    readonly nothingRecorded = computed(
        () => this.summary()?.allTime.dataQuality.state === 'empty',
    );

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

    /** `1 call`, `1,473 calls`. */
    fmtCount(n: number): string {
        return `${n.toLocaleString('en-US')} ${n === 1 ? 'call' : 'calls'}`;
    }

    /**
     * `+$1.23`, `−$1.23`, or `—` when it could not be priced. Whole dollars from $100, as the
     * Optimizer's figure was. The sign is the value's, never the rounding's, as the engine's own
     * text says it (`lumen_core::savings::text::usd_signed`): a loss under half a cent reads
     * `−$0.00`, and only zero itself reads `$0.00`.
     */
    fmtUsd(n: number | null): string {
        if (n === null) return '—';
        const abs = Math.abs(n);
        const digits = abs >= 100 ? abs.toFixed(0) : abs.toFixed(2);
        return n === 0 ? '$0.00' : `${n < 0 ? '−' : '+'}$${digits}`;
    }
}
