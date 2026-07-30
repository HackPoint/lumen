import { ChangeDetectionStrategy, Component, OnInit, computed, inject } from '@angular/core';
import { Cost } from '../../components/cost/cost';
import { Firefly } from '../../components/firefly/firefly';
import { LumenTooltip } from '../../directives/tooltip.directive';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';

@Component({
    selector: 'panel',
    imports: [Cost, Firefly, LumenTooltip],
    templateUrl: './panel.html',
    styleUrl: './panel.css',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Panel implements OnInit {
    readonly s = inject(SessionService);
    private readonly bridge = inject(TauriBridge);

    readonly fmtFill = computed(() => this.s.fill().toLocaleString());
    readonly fmtMax  = computed(() => this.s.maxContext().toLocaleString());
    readonly fillPct = computed(() => this.s.trayPercent());

    /** Explains why a project name is shown, and that other sessions may exist. */
    readonly projectHint = computed(() => {
        const n = this.s.sessionCount();
        return n > 1
            ? `Showing the most recently active of ${n} sessions: ${this.s.project()}. `
              + 'The gauge follows whichever window you are working in.'
            : `Project: ${this.s.project()}`;
    });

    ngOnInit(): void {
        void this.bridge.moveWindowToTray().catch(() => {});
        this.s.refreshFaultCount();
    }

    /**
     * Reveal the main window so the fault can actually be reviewed.
     *
     * The panel deliberately has no router links — it is a 320x400 popover — so this
     * hands off to the main window rather than trying to host the report here, where
     * the body could not be read before filing it.
     */
    openFaults(): void {
        this.s.openMainWindow();
    }
}
