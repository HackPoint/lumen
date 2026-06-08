import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { LumenTooltip } from '../../directives/tooltip.directive';

@Component({
    selector: 'gauge',
    imports: [LumenTooltip],
    templateUrl: './gauge.html',
    styleUrl: './gauge.scss',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Gauge {
    // --- inputs (contract: keep these three names) ---
    readonly fill = input(0);
    readonly max = input(200_000);
    readonly model = input('');

    // --- geometry: a 100×100 user-space so the SVG scales responsively ---
    readonly cx = 50;
    readonly cy = 50;
    readonly r = 40;
    readonly stroke = 7.5;
    readonly startAngle = 135; // open-bottom arc
    readonly sweep = 270;
    // With pathLength normalized to 100, the 270° band is 75 units long.
    readonly band = (this.sweep / 360) * 100;

    // --- derived state (semantics preserved) ---
    readonly ratio = computed(() => Math.max(0, Math.min(1, this.fill() / this.max())));
    readonly percent = computed(() => Math.round(this.ratio() * 100));

    // Compaction marker sits at 95% of the window — the alert threshold.
    readonly compRatio = 0.95;

    // Status by ratio (warn ≥ 0.80, alert ≥ 0.95) — same meaning as the data layer.
    readonly status = computed<'ok' | 'warn' | 'alert'>(() => {
        const r = this.ratio();
        if (r >= 0.95) return 'alert';
        if (r >= 0.8) return 'warn';
        return 'ok';
    });

    readonly fillColor = computed(() => {
        switch (this.status()) {
            case 'alert': return 'var(--lumen-alert)';
            case 'warn':  return 'var(--lumen-warn)';
            default:      return 'var(--lumen-ok)';
        }
    });

    // Reveal the arc from the start by shrinking the dash from the far end.
    readonly dashOffset = computed(() => this.band * (1 - this.ratio()));

    readonly markerX = computed(() => this.pointAt(this.compRatio)[0]);
    readonly markerY = computed(() => this.pointAt(this.compRatio)[1]);

    readonly fmtFill = computed(() => this.fill().toLocaleString());
    readonly fmtMax = computed(() => this.max().toLocaleString());

    private pointAt(t: number): [number, number] {
        const a = (this.startAngle + this.sweep * t) * (Math.PI / 180);
        return [this.cx + this.r * Math.cos(a), this.cy + this.r * Math.sin(a)];
    }
}
