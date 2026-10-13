import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { Gauge } from '../../components/gauge/gauge';
import { Firefly } from '../../components/firefly/firefly';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SessionService } from '../../session.service';
import { LumenTooltip } from '../../directives/tooltip.directive';
import type { ChannelBreakdown, ToolBreakdown } from '../../components/index';

/**
 * Details: the figures that answer none of Home's three questions but that someone checking
 * Lumen's working will want, moved here in 1.9.0.
 *
 * Two parts. Claude Code's context window (the gauge, its window selector and the alert banner
 * that describes it), which was the top of Home: it says how full Claude Code's context is,
 * which is not a saving and not something Lumen did. Then the Optimizer's own workings (the
 * share of tokens an intercepted read returns, the by-tool and by-channel breakdowns and how
 * many counts have a known provenance), which were on the Optimizer screen.
 */
@Component({
    selector: 'lumen-details',
    imports: [Gauge, Firefly, TabNav, LumenTooltip],
    templateUrl: './details.html',
    styleUrl: './details.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Details {
    readonly s = inject(SessionService);

    // ── context window ──────────────────────────────────────────────────────

    /** Explains which session the gauge is following, and that others exist. */
    readonly projectHint = computed(() => {
        const n = this.s.sessionCount();
        return n > 1
            ? `Showing the most recently active of ${n} sessions: ${this.s.project()}. `
              + 'The gauge follows whichever window you are working in, and the spend figures '
              + 'on Settings follow the same session.'
            : `Project: ${this.s.project()}`;
    });

    readonly activeIndex = computed(() => {
        const i = this.s.windowOptions.findIndex((o) => o.value === this.s.contextOverride());
        return i < 0 ? 0 : i;
    });

    // ── optimizer ───────────────────────────────────────────────────────────

    /** True once at least one lumen-optimized read has been recorded. */
    readonly hasData = computed(() => this.s.lifetimeOptimizedTokens() > 0);

    /** Effectiveness %, rounded to one decimal. 0 when no data yet (never NaN). */
    readonly effectPct = computed(() => {
        const p = this.s.effectivenessPct();
        return isFinite(p) && p > 0 ? Math.round(p * 10) / 10 : 0;
    });

    readonly maxToolSaved = computed(() =>
        Math.max(1, ...this.s.optimizedByTool().map((r) => r.savedTokens)),
    );

    readonly maxChanSaved = computed(() =>
        Math.max(1, ...this.s.optimizedByChannel().map((r) => r.savedTokens)),
    );

    barPct(val: number, max: number): number {
        return max > 0 ? Math.round((val / max) * 100) : 0;
    }

    toolLabel(row: ToolBreakdown): string {
        switch (row.tool) {
            case 'mcp__lumen__smart_read':    return 'smart_read';
            case 'mcp__lumen__recall_file':   return 'recall_file';
            case 'mcp__lumen__compress_logs': return 'compress_logs';
            default:                           return row.tool;
        }
    }

    chanLabel(row: ChannelBreakdown): string {
        switch (row.channel) {
            case 'cli':     return 'CLI (Full mode)';
            case 'vscode':  return 'VS Code (Soft mode)';
            default:        return row.channel;
        }
    }

    fmtTokens(n: number): string {
        return n.toLocaleString();
    }
}
