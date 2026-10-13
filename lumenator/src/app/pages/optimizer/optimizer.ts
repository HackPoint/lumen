import {
    ChangeDetectionStrategy,
    Component,
    OnInit,
    computed,
    inject,
} from '@angular/core';
import { RouterLink } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';
import { IdentityHookPrompt } from '../../components/identity-hook-prompt/identity-hook-prompt';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SessionService } from '../../session.service';
import { RepeatReadsService } from '../../repeat-reads.service';
import { RATE } from '../../components/index';
import { LumenTooltip } from '../../directives/tooltip.directive';

/**
 * What Lumen caused, beside what Claude Code's cache did, and whether Lumen is working.
 *
 * Until 1.9.0 this screen led with a net dollar value of interception. It is gone until the
 * savings engine replaces it with its own figures; the token ratio, the by-tool and by-channel
 * breakdowns and the provenance counts it carried are on Details.
 */
@Component({
    selector: 'optimizer',
    imports: [RouterLink, Firefly, IdentityHookPrompt, TabNav, LumenTooltip],
    templateUrl: './optimizer.html',
    styleUrl: './optimizer.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Optimizer implements OnInit {
    /**
     * Wording for the effectiveness claim, qualified when provenance is unknown.
     *
     * The old copy said "measured to the token, never estimated" unconditionally.
     * That was false on any install whose baked tokenizer path had died: the hook
     * substituted bytes/4 without recording that it had. The claim is now made only
     * over data that can support it.
     */
    readonly provenanceNote = computed(() =>
        this.s.unverifiedProvenanceRows() === 0
            ? 'Counted to the token by a local tokenizer, never estimated.'
            : 'Counts are exact for Lumen tool calls; some older events have unverified provenance.',
    );

    readonly savingsNote = computed(() =>
        this.s.unverifiedProvenanceRows() === 0
            ? 'caused · measured · verifiable · grows with use'
            : 'caused · partly unverified · grows with use',
    );

    readonly s = inject(SessionService);
    readonly rr = inject(RepeatReadsService);

    ngOnInit(): void {
        // This page's own figures arrive through the daemon stream, and the navigation reads
        // the fault count. The repeat reads card says whether the settings file turns them
        // off, so it reads that file.
        void this.rr.load();
    }

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

    // ── repeat reads (answers in none of the totals above) ────────────────────

    /** The figures with the two sums the card needs. Null from a backend older than 1.8.0: no card then. */
    readonly repeatCard = computed(() => {
        const r = this.s.optimizerStats()?.repeatReads;
        if (!r) return null;
        const served = r.unchangedServed + r.deltasServed;
        return { ...r, served, asked: served + r.freshRequests };
    });

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
