import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RATE } from '../index';
import type { TokenAgg, UsageReport } from '../index';
import { LumenTooltip } from '../../directives/tooltip.directive';

/**
 * Dumb presentational view of the Usage & Cost report. Input-driven; all dollar
 * figures are derived here from the single shared RATE table (never in SQL).
 *
 * HONEST FRAMING:
 *  - rolling windows are "used in last 5h / 7d" — consumption, NOT % of plan.
 *  - the 5h reset shown is a proxy (window start + 5h), tagged "approx".
 *  - "Saved by caching" is reported (Claude Code caches; Lumen only sums it).
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

    /** Cost of an aggregate at the single RATE table (USD). */
    private cost(a: TokenAgg): number {
        return (
            a.input * RATE.input +
            a.output * RATE.output +
            a.cacheRead * RATE.cacheRead +
            a.cacheWrite * RATE.cacheWrite
        );
    }

    // rolling 5h
    readonly cost5h = computed(() => this.cost(this.report().rolling5h));

    // rolling 7d (opus + other combined, plus the split for display)
    readonly cost7d = computed(() =>
        this.cost(this.report().rolling7dOpus) + this.cost(this.report().rolling7dOther),
    );
    readonly tokens7d = computed(
        () => this.report().rolling7dOpus.totalTokens + this.report().rolling7dOther.totalTokens,
    );
    readonly turns7d = computed(
        () => this.report().rolling7dOpus.turns + this.report().rolling7dOther.turns,
    );

    // calendar rollups
    readonly costToday = computed(() => this.cost(this.report().today));
    readonly costWeek = computed(() => this.cost(this.report().thisWeek));
    readonly costAll = computed(() => this.cost(this.report().allTime));

    // Lifetime "Saved by caching": cache-read tokens valued at what they WOULD
    // have cost at full input price, minus their (cheaper) cache-read price.
    //   saved = cacheRead * (RATE.input − RATE.cacheRead)
    readonly savedByCaching = computed(
        () => this.report().allTime.cacheRead * (RATE.input - RATE.cacheRead),
    );

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

    readonly fmt = (n: number) => '$' + n.toFixed(2);
    readonly fmtTokens = (n: number) => n.toLocaleString();
}
