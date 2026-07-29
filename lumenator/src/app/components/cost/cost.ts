import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RATE } from '../index';
import type { CostTotals } from '../index';
import { LumenTooltip } from '../../directives/tooltip.directive';

@Component({
    selector: 'cost',
    imports: [LumenTooltip],
    templateUrl: './cost.html',
    styleUrl: './cost.scss',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Cost {
    /** Cumulative token totals for the active session (or all sessions). */
    readonly totals = input.required<CostTotals>();

    // --- real cost in USD ---
    readonly costInput = computed(() => this.totals().input * RATE.input);
    readonly costOutput = computed(() => this.totals().output * RATE.output);
    readonly costCacheRead = computed(() => this.totals().cacheRead * RATE.cacheRead);
    readonly costCacheWrite = computed(() => this.totals().cacheWrite * RATE.cacheWrite);

    readonly totalCost = computed(
        () => this.costInput() + this.costOutput() + this.costCacheRead() + this.costCacheWrite(),
    );

    /**
     * Whether a formatted figure needs a reduced size to fit its column.
     *
     * CSS cannot size text by its own character count, and the popover column is
     * only ~107px wide, so `$1134.87` sat flush against the edge and anything
     * longer overflowed. Measured thresholds rather than guessed: 8 characters is
     * where the default 1.15rem stops fitting.
     */
    isLong(formatted: string): boolean {
        return formatted.length >= 8 && formatted.length < 10;
    }

    isXLong(formatted: string): boolean {
        return formatted.length >= 10;
    }

    // --- the savings story ---
    // What the cache-read tokens WOULD have cost at full input price, vs what they did cost.
    readonly cacheReadFullPrice = computed(() => this.totals().cacheRead * RATE.input);
    readonly cacheSavings = computed(() => this.cacheReadFullPrice() - this.costCacheRead());

    // hit rate = cacheRead / (cacheRead + cacheWrite + fresh input)
    readonly cacheHitRate = computed(() => {
        const t = this.totals();
        const denom = t.cacheRead + t.cacheWrite + t.input;
        return denom > 0 ? t.cacheRead / denom : 0;
    });

    readonly fmt = (n: number) => '$' + n.toFixed(2);
    readonly fmtPct = (n: number) => Math.round(n * 100) + '%';
}
