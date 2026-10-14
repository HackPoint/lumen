import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { Router } from '@angular/router';
import type { Recommendation, SavingsSummary } from '../../savings-summary';

/** A piece of the recommendation's text: `code` where the engine wrote it in backticks. */
export interface TextPart {
    text: string;
    code: boolean;
}

/** The recommendations whose next step is `lumen doctor`, run in a terminal. */
const DOCTOR: ReadonlyArray<Recommendation['code']> = ['blocksWithoutSavings', 'noCalls', 'unplacedCalls'];

/**
 * Under Home's headline: is it working, and what to do. One status line and at most one
 * recommendation, both the summary's own words, so a loss or a period with nothing to show says
 * why where the figure is.
 *
 * The recommendation carries the step it implies: Setup for a hook that does not reach the
 * server (the button goes there, as the identity hook prompt's does), and `lumen doctor` for the
 * findings it explains. Renders nothing without a summary.
 */
@Component({
    selector: 'lumen-savings-status',
    templateUrl: './savings-status.html',
    styleUrl: './savings-status.scss',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SavingsStatus {
    private readonly router = inject(Router);

    readonly summary = input<SavingsSummary | null>(null);

    /** The status in the words `lumen report` uses for it. */
    readonly line = computed(() => {
        const st = this.summary()?.status;
        if (!st) return null;
        switch (st.state) {
            case 'working':
                return 'Status: working';
            case 'notWorking':
                return `Status: not working: ${st.reason}`;
            case 'noData':
                return 'Status: no data';
        }
    });

    readonly state = computed(() => this.summary()?.status.state ?? null);

    readonly recommendation = computed(() => this.summary()?.recommendation ?? null);

    readonly parts = computed<TextPart[]>(() => {
        const r = this.recommendation();
        return r ? split(r.text) : [];
    });

    readonly runsSetup = computed(() => this.recommendation()?.code === 'identityHook');

    readonly runsDoctor = computed(() => {
        const r = this.recommendation();
        return r !== null && DOCTOR.includes(r.code);
    });

    runSetup(): void {
        void this.router.navigate(['/setup']);
    }
}

/** The text with each `backticked` span apart, as code. An unpaired backtick stays as text. */
export function split(text: string): TextPart[] {
    const out: TextPart[] = [];
    const re = /`([^`]+)`/g;
    let at = 0;
    for (let m = re.exec(text); m !== null; m = re.exec(text)) {
        if (m.index > at) out.push({ text: text.slice(at, m.index), code: false });
        out.push({ text: m[1], code: true });
        at = m.index + m[0].length;
    }
    if (at < text.length) out.push({ text: text.slice(at), code: false });
    return out;
}
