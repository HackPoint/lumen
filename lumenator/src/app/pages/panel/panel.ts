import {
    AfterViewInit,
    ChangeDetectionStrategy,
    Component,
    ElementRef,
    OnDestroy,
    OnInit,
    computed,
    inject,
} from '@angular/core';
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
export class Panel implements OnInit, AfterViewInit, OnDestroy {
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
        // From the panel, not the main window: the popover is what actually gets opened,
        // and the backend throttles to one check a day regardless of how often this runs.
        this.s.checkForUpdate();
    }

    private readonly host = inject(ElementRef);
    private observer?: ResizeObserver;
    /** Last height requested, so a no-op resize is not sent on every observation. */
    private requested = 0;

    /**
     * Keep the window the size of the card.
     *
     * The popover was a fixed 320x400 whose card clipped the overflow, so every
     * conditional row added pushed the one below it further under the bottom edge until
     * the fault button was off-screen entirely — rendered, styled and invisible. The card
     * is content-sized now and the window follows it.
     */
    ngAfterViewInit(): void {
        const card = (this.host.nativeElement as HTMLElement).querySelector('.panel');
        if (!(card instanceof HTMLElement)) return;

        // Measured once up front, before the observer: jsdom has no ResizeObserver, and a
        // test build must still exercise this path.
        this.fit(card);
        if (typeof ResizeObserver === 'undefined') return;
        this.observer = new ResizeObserver(() => this.fit(card));
        this.observer.observe(card);
    }

    ngOnDestroy(): void {
        this.observer?.disconnect();
    }

    /**
     * Request a window height matching the card plus its 8px inset on each side.
     *
     * No feedback loop: the card's height comes from its content, not from the window, so
     * resizing the window does not change what is being measured. The threshold guards
     * against sub-pixel jitter rather than against recursion.
     */
    private fit(card: HTMLElement): void {
        const height = Math.ceil(card.getBoundingClientRect().height) + 16;
        if (Math.abs(height - this.requested) < 2) return;
        this.requested = height;
        this.s.resizePanel(height);
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
