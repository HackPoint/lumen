import {
    ChangeDetectionStrategy,
    Component,
    computed,
    inject,
    OnInit,
} from '@angular/core';
import { RouterLink, RouterLinkActive } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';
import { SessionService } from '../../session.service';
import { LumenTooltip } from '../../directives/tooltip.directive';
import type { FileHotspot } from '../../components/index';

/**
 * Where this project's context actually goes.
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
    selector: 'hotspots',
    imports: [RouterLink, RouterLinkActive, Firefly, LumenTooltip],
    templateUrl: './hotspots.html',
    styleUrl: './hotspots.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Hotspots implements OnInit {
    readonly s = inject(SessionService);

    ngOnInit(): void {
        this.s.refreshContextReport();
        this.s.refreshFaultCount();
    }

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

    // ── Fault report ──────────────────────────────────────────────────────────
    //
    // Deliberately two clicks. The first renders locally and shows the exact text; the
    // second publishes it to a public tracker. Collapsing them into one button would
    // make an irreversible outward-facing action the default outcome of curiosity.

    readonly report = computed(() => this.s.faultReport());
    readonly reportLoading = computed(() => this.s.faultReportLoading());
    readonly reportNone = computed(() => this.s.faultsNone());
    readonly filing = computed(() => this.s.faultFiling());
    readonly filed = computed(() => this.s.faultFiled());
    /** True once something actually exists on the tracker — a handoff does not count. */
    readonly published = computed(() => {
        const f = this.filed();
        return f !== null && !f.handoff;
    });
    readonly reportError = computed(() => this.s.faultError());

    /** Whether this body has already been sent somewhere — sending twice is pointless. */
    readonly alreadyFiled = computed(() => this.filed() !== null);

    preview(): void {
        this.s.refreshFaultReport();
    }

    file(): void {
        this.s.fileFaultReport();
    }

    dismiss(): void {
        this.s.dismissFaultReport();
    }
}
