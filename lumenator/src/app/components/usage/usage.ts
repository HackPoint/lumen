import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import type { UsageReport } from '../index';
import { aggTokens, figureTag, fmtUsd, price } from '../../prices';
import type { FigureTag, PriceTable, Priced } from '../../prices';
import { LumenTooltip } from '../../directives/tooltip.directive';

/**
 * Dumb presentational view of the Usage & Cost report. Input-driven; every dollar figure is
 * priced here, each model's share at that model's rates from the `lumen_prices` table, and
 * never in SQL.
 *
 * HONEST FRAMING:
 *  - rolling windows are "used in last 5h / 7d" — consumption, NOT % of plan.
 *  - the 5h reset shown is a proxy (window start + 5h), tagged "approx".
 *  - "Saved by caching" is reported (Claude Code caches; Lumen only sums it).
 *  - a figure that is not the whole (a model with no price, a speed or a cache write's
 *    duration not recorded) says so, and why.
 */
@Component({
    selector: 'usage',
    imports: [LumenTooltip],
    templateUrl: './usage.html',
    styleUrl: './usage.scss',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Usage {
    readonly report = input.required<UsageReport>();

    /** The price table; null until it loads, and then no figure is shown. */
    readonly prices = input<PriceTable | null>(null);

    // rolling 5h
    readonly priced5h = computed(() => price(aggTokens(this.report().rolling5h), this.prices()));
    readonly cost5h = computed(() => this.priced5h().total);

    // rolling 7d (opus + other combined, plus the split for display)
    readonly priced7d = computed(() =>
        price(
            [...aggTokens(this.report().rolling7dOpus), ...aggTokens(this.report().rolling7dOther)],
            this.prices(),
        ),
    );
    readonly cost7d = computed(() => this.priced7d().total);
    readonly tokens7d = computed(
        () => this.report().rolling7dOpus.totalTokens + this.report().rolling7dOther.totalTokens,
    );
    readonly turns7d = computed(
        () => this.report().rolling7dOpus.turns + this.report().rolling7dOther.turns,
    );

    // calendar rollups
    readonly pricedToday = computed(() => price(aggTokens(this.report().today), this.prices()));
    readonly pricedWeek = computed(() => price(aggTokens(this.report().thisWeek), this.prices()));
    readonly pricedAll = computed(() => price(aggTokens(this.report().allTime), this.prices()));
    readonly costToday = computed(() => this.pricedToday().total);
    readonly costWeek = computed(() => this.pricedWeek().total);
    readonly costAll = computed(() => this.pricedAll().total);

    // Lifetime "Saved by caching": each model's cache-read tokens valued at what they WOULD
    // have cost at its full input price, less its (cheaper) cache-read price.
    readonly savedByCaching = computed(() => this.pricedAll().cacheSavings);

    /** Approx 5h reset, formatted to local time (or null). */
    readonly resetApproxLocal = computed(() => {
        const r = this.report().resetApprox;
        if (!r) return null;
        // SQL gives "YYYY-MM-DD HH:MM:SS" in UTC; mark it UTC then localize.
        const d = new Date(r.replace(' ', 'T') + 'Z');
        return isNaN(d.getTime())
            ? null
            : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    });

    /** A total, or a dash when none of the tokens behind it has a price. */
    readonly money = (priced: Priced) => fmtUsd(priced, 'total');
    /** What a total that is not the whole carries; null when it is exact. */
    readonly tag = (priced: Priced): FigureTag | null => figureTag(priced, 'total');
    /** The lifetime caching saving, or a dash. */
    readonly savedMoney = (priced: Priced) => fmtUsd(priced, 'savings');
    readonly savedTag = (priced: Priced): FigureTag | null => figureTag(priced, 'savings');
    readonly fmtTokens = (n: number) => n.toLocaleString();
}
