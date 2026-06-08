import {
    ChangeDetectionStrategy,
    Component,
    computed,
    inject,
} from '@angular/core';
import { RouterLink, RouterLinkActive } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';
import { SessionService } from '../../session.service';
import { RATE } from '../../components/index';
import type { ChannelBreakdown, ToolBreakdown } from '../../components/index';

@Component({
    selector: 'optimizer',
    imports: [RouterLink, RouterLinkActive, Firefly],
    templateUrl: './optimizer.html',
    styleUrl: './optimizer.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Optimizer {
    readonly s = inject(SessionService);

    // ── hero ──────────────────────────────────────────────────────────────────

    /** Effectiveness %, rounded to one decimal. 0 when no data yet (never NaN). */
    readonly effectPct = computed(() => {
        const p = this.s.effectivenessPct();
        return isFinite(p) && p > 0 ? Math.round(p * 10) / 10 : 0;
    });

    /** True once at least one lumen-optimized read has been recorded. */
    readonly hasData = computed(() => this.s.lifetimeOptimizedTokens() > 0);

    // ── firefly + mode banner ─────────────────────────────────────────────────

    readonly fireflyState = computed((): 'full' | 'soft' | 'idle' => {
        switch (this.s.currentChannel()) {
            case 'cli':    return 'full';
            case 'vscode': return 'soft';
            default:       return 'idle';
        }
    });

    readonly modeLabel = computed(() => {
        switch (this.s.currentChannel()) {
            case 'cli':    return 'Full mode';
            case 'vscode': return 'Soft mode';
            default:       return 'No activity yet';
        }
    });

    readonly modeDesc = computed(() => {
        switch (this.s.currentChannel()) {
            case 'cli':
                return 'Reads intercepted by Lumen — savings and missed reads both tracked.';
            case 'vscode':
                return 'Lumen tools available but not enforced. Only optimized reads tracked here. Use the CLI for guaranteed optimization.';
            default:
                return 'Run a task that reads a large file. Use the CLI for full interception.';
        }
    });

    /** Hero accent color follows the gauge status palette. */
    readonly heroColor = computed(() => {
        const p = this.effectPct();
        if (p >= 80) return 'var(--lumen-ok)';
        if (p >= 50) return 'var(--lumen-warn)';
        return 'var(--lumen-text-dim)';
    });

    // ── D2: caching savings (REPORTED by Claude Code, NOT caused by Lumen) ────
    //
    // Derived from the same usage signal used by the Usage component.
    // Formula: allTime.cacheRead * (RATE.input − RATE.cacheRead)
    // This is the price differential — what those tokens WOULD have cost at
    // full input price versus the discounted cache-read price.
    //
    // NEVER merged with lifetimeOptimizedUsd (D2 is reported; E5 is caused).

    readonly cacheSavedTokens = computed(() =>
        this.s.usage()?.allTime?.cacheRead ?? 0,
    );

    readonly cacheSavedUsd = computed(() =>
        this.cacheSavedTokens() * (RATE.input - RATE.cacheRead),
    );

    // ── missed reads (CLI only) ───────────────────────────────────────────────

    /** Show only in CLI mode when missed reads have been recorded. */
    readonly showMissed = computed(() =>
        this.s.currentChannel() === 'cli' && this.s.missedReads().calls > 0,
    );

    // ── breakdown bar helpers ─────────────────────────────────────────────────

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

    // ── formatting ────────────────────────────────────────────────────────────

    fmtTokens(n: number): string {
        return n.toLocaleString();
    }

    /** Auto-scale USD to meaningful precision (tokens saved can be tiny). */
    fmtUsd(n: number): string {
        if (n >= 10)   return '$' + n.toFixed(2);
        if (n >= 0.01) return '$' + n.toFixed(4);
        return '$' + n.toFixed(6);
    }
}
