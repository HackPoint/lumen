import {
    ChangeDetectionStrategy,
    Component,
    computed,
    inject,
} from '@angular/core';
import { Firefly } from '../../components/firefly/firefly';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SessionService } from '../../session.service';
import { LumenTooltip } from '../../directives/tooltip.directive';
import { recommendations } from '../../recommendation';
import type { FileHotspot } from '../../components/index';

/**
 * Why: what to do about where the context goes, then where it goes, in every project the ledger
 * holds. Hotspots until 1.9.0.
 *
 * In the navigation only while there is something to do (see `isRecommendation` for the rule),
 * and reachable by its URL at any time, when it says there is nothing to act on.
 *
 * Diagnosis, not savings — and the distinction is the point. Every other figure in the
 * product claims a benefit and therefore has to defend a counterfactual; this one only
 * reports what was read. It costs no tokens, intercepts nothing and forces no rounds, so
 * it is the one number here that cannot come out negative.
 *
 * The copy is deliberately free of percentages-saved language. "Here is where your
 * context goes" survives contact with the data in a way that "we saved you X%" has not.
 */
@Component({
    selector: 'why',
    imports: [Firefly, TabNav, LumenTooltip],
    templateUrl: './why.html',
    styleUrl: './why.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Why {
    // The report is fetched by the navigation on this page, which needs it to decide whether
    // this screen is in it; fetching it here as well would ask the ledger twice per visit.
    readonly s = inject(SessionService);

    /** The files to act on, with the backend's advice for each. */
    readonly todo = computed(() => recommendations(this.s.contextReport()));

    readonly files = computed(() => this.s.contextFiles());
    readonly hasData = computed(() => this.s.contextTotalTokens() > 0);

    /** Bar width as a share of the largest file, so the top row always fills. */
    barPct(f: FileHotspot): number {
        const top = this.files()[0]?.totalTokens ?? 0;
        return top > 0 ? Math.max(2, (100 * f.totalTokens) / top) : 0;
    }

    fmt(n: number): string {
        return n.toLocaleString('en-US');
    }

    /** 3.83M rather than 3,833,250 — the row is a ranking, not an audit. */
    compact(n: number): string {
        if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
        if (n >= 1_000) return `${(n / 1_000).toFixed(0)}k`;
        return `${n}`;
    }

    /**
     * How much of this file's reading found it unchanged.
     *
     * Shown as a share because the absolute count is meaningless without the total: 4
     * unchanged reads of 6 is a pattern, 4 of 400 is noise.
     */
    unchangedPct(f: FileHotspot): number {
        return f.reads > 0 ? (100 * f.unchangedRereads) / f.reads : 0;
    }
}
