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
import { aggTokens, figureTag, price } from '../../prices';
import { LumenTooltip } from '../../directives/tooltip.directive';

/**
 * How far a cl100k_base count is from Claude's own: the pooled rows of
 * lumen_core::calibration for the two tokenizers it found, measured by
 * benchmarks/calibration against Claude's usage data on raw file text. A Rust test holds
 * this text to those rows, so a re-run that moves a ratio fails until the sentence moves
 * with it.
 */
export const CALIBRATION_NOTE =
    'On raw file text, the models measured from Claude Opus 4.7 on (Opus 4.7, Opus 4.8, Opus 5, '
    + 'Sonnet 5, Fable 5, Opus 5.5, Sonnet 5.5 and Haiku 5.5) count about 1.59× as many tokens '
    + '(1.44–1.70×), and the earlier models measured (Opus 4.6, Sonnet 4.6, Opus 4.5 and Haiku '
    + '4.5) about 1.24× as many (1.14–1.31×): the 10th to 90th percentile of 60 samples measured '
    + "against Claude's own usage data (calibration of 2026-10-14). A Read's line numbers and the "
    + 'tool-result wrapper around every answer are not in these ratios.';

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
     * An earlier copy claimed every count measured, unconditionally. That was false on
     * any install whose baked tokenizer path had died: the hook substituted bytes/4
     * without recording that it had. The claim is now made only over data that can
     * support it.
     *
     * Until 1.9.0 it also called the count exact: the copy before 1.9.0 read "Counted
     * to the token by a local tokenizer, never estimated." It was exact only in
     * cl100k_base's units. The note now says what counted it and how far that is from
     * Claude's own count, quoting the pooled row of lumen_core::calibration (a Rust test
     * holds this text to that row).
     */
    readonly provenanceNote = computed(() =>
        (this.s.unverifiedProvenanceRows() === 0
            ? "Counted with cl100k_base, a local stand-in for Claude's tokenizer."
            : "Lumen tool calls are counted with cl100k_base, a local stand-in for Claude's "
              + 'tokenizer; some older events have unverified provenance.')
        + ' ' + CALIBRATION_NOTE,
    );

    readonly savingsNote = computed(() =>
        this.s.unverifiedProvenanceRows() === 0
            ? 'caused · counted locally · verifiable · grows with use'
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
    // Derived from the same usage signal used by the Usage component, each model's
    // cache reads at that model's prices: input price less cache-read price, which is
    // what those tokens WOULD have cost at full price versus what they did cost.
    //
    // NEVER merged with the optimizer's figures (D2 is reported; E5 is caused).

    readonly cacheSavedTokens = computed(() =>
        this.s.usage()?.allTime?.cacheRead ?? 0,
    );

    readonly cacheSavedPriced = computed(() => {
        const all = this.s.usage()?.allTime;
        return price(all ? aggTokens(all) : [], this.s.prices());
    });

    readonly cacheSavedUsd = computed(() => this.cacheSavedPriced().cacheSavings);

    /** What the saving carries when it is not the whole ('at least' or 'no price'), and why. */
    readonly cacheSavedTag = computed(() => figureTag(this.cacheSavedPriced(), 'savings'));

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
