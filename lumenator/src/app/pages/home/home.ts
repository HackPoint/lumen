import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, effect, inject, signal, untracked } from '@angular/core';
import { Router } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';
import { IdentityHookPrompt } from '../../components/identity-hook-prompt/identity-hook-prompt';
import { BillingPrompt } from '../../components/billing-prompt/billing-prompt';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SavingsHeadline } from '../../components/savings-headline/savings-headline';
import { SavingsStatus } from '../../components/savings-status/savings-status';
import { RepeatReadsService } from '../../repeat-reads.service';
import { SessionService } from '../../session.service';
import { BillingModeService } from '../../billing-mode.service';
import { TauriBridge } from '../../tauri-bridge';

/** Mirrors the `lumen_startup_health` command's payload. */
interface StartupHealth {
    degraded: boolean;
    tray: string;
    degradations: string[];
    /** The hidden-icon preferences this launch cleared. A backend before 1.6.0 omits it. */
    restored?: string[];
}

/**
 * The main window's first screen, which answers three questions only: how much Lumen saved (the
 * savings headline, in bold), is it working and what to do (one status line and at most one
 * recommendation under it), all from the engine's savings summary.
 *
 * Everything else that used to be here has a place of its own since 1.9.0: the context gauge
 * and its window selector are on Details, with the alert banner that describes them; the spend
 * figures, the alert thresholds, the Repeat reads switch and About are on Settings. Each answers
 * a question about Claude Code or about Lumen's configuration, not about savings.
 */
@Component({
    selector: 'home',
    imports: [Firefly, IdentityHookPrompt, BillingPrompt, TabNav, SavingsHeadline, SavingsStatus],
    templateUrl: './home.html',
    styleUrl: './home.css',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Home implements OnInit {
    /** The switch the identity hook prompt reads; this page loads it, as the prompt expects. */
    readonly rr = inject(RepeatReadsService);
    private readonly router = inject(Router);
    private readonly bridge = inject(TauriBridge);
    private readonly destroyRef = inject(DestroyRef);

    /**
     * What failed at startup, if anything.
     *
     * Reported because a degraded Lumen used to look identical to a healthy one. In issue #5 the
     * tray never appeared, nothing errored, and the app carried on presenting itself as fine —
     * so the user had no way to know what was wrong or that a report was worth filing.
     */
    readonly health = signal<StartupHealth | null>(null);

    /**
     * The hidden-icon preferences this launch cleared, if any.
     *
     * macOS keeps a status item hidden on every launch once it has been ⌘-dragged off the menu
     * bar. Lumen clears that and opens this window for it; the window has to say why, or the icon
     * coming back reads as the app overriding the user, who is never told how to stop it.
     */
    readonly restored = signal<string[]>([]);

    /**
     * The version this build was made as, as the Tauri runtime reports it, or null until it has.
     *
     * Read from the runtime and never written down here: the one place a version is declared is
     * `tauri.conf.json`, and a copy in this file would go stale at the next release without
     * anything failing. The tray menu shows the same value from the same source.
     */
    readonly version = signal<string | null>(null);

    /**
     * True once the backend has said Setup is done. The billing question waits for it: it is
     * asked after Setup and never instead of it, and an answer that cannot be had keeps it away.
     */
    readonly setupDone = signal(false);

    /**
     * What the headline shows: the engine's summary, as `lumen_savings_summary` last gave it. The
     * service fetches it when the app starts, every minute after and whenever the window gets
     * focus; null until the first fetch succeeds, and the headline then renders nothing.
     */
    private readonly session = inject(SessionService);
    readonly savings = this.session.savingsSummary;
    /** Why the latest fetch failed: with a summary, it is shown stale; with none, no figure. */
    readonly savingsError = this.session.savingsError;
    private readonly billing = inject(BillingModeService);

    constructor() {
        // A billing choice made here or on Settings changes how the summary says its dollars, and
        // the summary reads it from the settings file: fetched again once the file says another.
        let seen: string | null | undefined;
        effect(() => {
            const mode = this.billing.setting()?.mode;
            if (mode === undefined) return;
            if (seen !== undefined && seen !== mode) untracked(() => this.session.refreshSavingsSummary());
            seen = mode;
        });
    }

    ngOnInit(): void {
        // Subscribed before the first read. A reason that arrives later, such as the tray checks
        // giving up six seconds in, comes with this event, and the window opened for it is this
        // one: read once at load, it opened blank.
        const sub = this.bridge.listen$('startup-health').subscribe(() => this.readHealth());
        this.destroyRef.onDestroy(() => sub.unsubscribe());
        this.readHealth();
        this.readVersion();
        this.bridge.invoke<boolean>('lumen_setup_needed')
            .then(needed => {
                if (needed) this.router.navigate(['/setup']);
                else if (needed === false) this.setupDone.set(true);
            })
            .catch(() => { /* non-fatal: proceed normally if command fails */ });
        void this.rr.load();
        // Coming back to Home from another screen shows the figures as they are now. On the
        // first open the service's own first fetch is still out.
        if (this.savings() !== null || this.savingsError() !== null) this.session.refreshSavingsSummary();
    }

    private readVersion(): void {
        this.bridge.getVersion()
            .then(v => this.version.set(v))
            // Non-fatal: the header leaves the line out, and Settings' About says it is unavailable.
            .catch(() => { /* nothing to show here */ });
    }

    private readHealth(): void {
        this.bridge.invoke<StartupHealth>('lumen_startup_health')
            .then(h => {
                if (h?.degraded) this.health.set(h);
                if (h?.restored?.length) this.restored.set(h.restored);
            })
            .catch(() => { /* non-fatal: an older backend simply has no such command */ });
    }
}
