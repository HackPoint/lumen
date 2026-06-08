import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RATE } from '../index';
import type { CostTotals } from '../index';

@Component({
    selector: 'cost',
    imports: [],
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
