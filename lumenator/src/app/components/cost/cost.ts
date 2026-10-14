import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import type { CostTotals } from '../index';
import { figureTag, fmtUsd } from '../../prices';
import type { Figure, FigureTag, Priced } from '../../prices';
import { LumenTooltip } from '../../directives/tooltip.directive';

@Component({
    selector: 'cost',
    imports: [LumenTooltip],
    templateUrl: './cost.html',
    styleUrl: './cost.scss',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Cost {
    /** Cumulative token totals for the active session (or all sessions), for the hit rate. */
    readonly totals = input.required<CostTotals>();

    /**
     * The same tokens priced, each model's at that model's rates and speed. Priced by the
     * caller from the `lumen_prices` table: this component has no price of its own.
     */
    readonly priced = input.required<Priced>();

    // --- real cost in USD ---
    readonly costInput = computed(() => this.priced().input);
    readonly costOutput = computed(() => this.priced().output);
    readonly costCacheRead = computed(() => this.priced().cacheRead);
    readonly costCacheWrite = computed(() => this.priced().cacheWrite);

    readonly totalCost = computed(() => this.priced().total);

    /**
     * The tag a figure here carries when it is not the whole ('at least' or 'no price'), with
     * the reason; null when it is exact. Each figure has its own, since a write of unknown
     * duration, say, makes the cache-write cost a floor but not the output cost.
     */
    tag(figure: Figure): FigureTag | null {
        return figureTag(this.priced(), figure);
    }

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
    // What the cache-read tokens WOULD have cost at their model's full input price, less
    // what they did cost.
    readonly cacheSavings = computed(() => this.priced().cacheSavings);

    // hit rate = cacheRead / (cacheRead + cacheWrite + fresh input)
    readonly cacheHitRate = computed(() => {
        const t = this.totals();
        const denom = t.cacheRead + t.cacheWrite + t.input;
        return denom > 0 ? t.cacheRead / denom : 0;
    });

    readonly fmt = (n: number) => '$' + n.toFixed(2);
    /** A figure from this panel, or a dash when none of its tokens has a price. */
    money(figure: Figure): string {
        return fmtUsd(this.priced(), figure);
    }
    readonly fmtPct = (n: number) => Math.round(n * 100) + '%';
}
