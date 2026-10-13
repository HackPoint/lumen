import { ChangeDetectionStrategy, Component, HostListener, OnInit, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import { RepeatReadsService } from '../../repeat-reads.service';
import { TauriBridge } from '../../tauri-bridge';
import type { IdentityHookStatus } from '../index';

/**
 * Says that repeat reads are on and cannot work yet, for an install that Setup has not given the
 * hook that identifies a conversation.
 *
 * 1.8.0 turns repeat reads on, and the server can only tell which conversation a call belongs to
 * through one hook in Claude Code's settings. Setup registers it, but Setup runs by itself only
 * until its marker exists, so an install set up under 1.7.0 or earlier is never sent back: the
 * switch says on, every read is answered in full, and nothing says why. This says why, and its
 * button goes to Setup, which runs its steps when it opens.
 *
 * It is a prompt and not a banner for a fault, so it shows only while there is something to
 * do. Setup still to run is the Setup page's to say, a hook that is registered (or reaching the
 * server by way of a plugin) needs nothing, and with repeat reads off there is nothing to wait
 * for. What cannot be known leaves it hidden: an unknown is not an accusation, and the
 * switch's state is awaited, not guessed, so the prompt does not flash and withdraw.
 *
 * Hosted by the pages that look after the install (Home and Optimizer), which read the
 * repeat-reads setting when they open. Not by Setup, which is the fix, nor the tray popover.
 */
@Component({
    selector: 'identity-hook-prompt',
    templateUrl: './identity-hook-prompt.html',
    styleUrl: './identity-hook-prompt.scss',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class IdentityHookPrompt implements OnInit {
    private readonly bridge = inject(TauriBridge);
    private readonly router = inject(Router);
    /** The switch as the settings file holds it, read by the host page; never read here. */
    private readonly rr = inject(RepeatReadsService);

    /** What the backend last said about the hook; null until it has, and when it could not. */
    private readonly status = signal<IdentityHookStatus | null>(null);

    /** Whether Setup is still to run; null until known, and when the backend could not say. */
    private readonly setupNeeded = signal<boolean | null>(null);

    /** How many checks were started. An answer is kept only if no later check was started. */
    private started = 0;

    readonly visible = computed(() => {
        const status = this.status();
        return (
            this.setupNeeded() === false &&
            status !== null &&
            !status.registered &&
            !status.servedElsewhere &&
            this.rr.setting()?.on === true
        );
    });

    ngOnInit(): void {
        void this.check();
    }

    /**
     * Asked again when the window gets focus, so the prompt clears when Setup was run and the
     * user comes back to it from another window. Only while it is showing: once it is gone,
     * there is nothing a later answer could change.
     */
    @HostListener('window:focus')
    onFocus(): void {
        if (this.visible()) void this.check();
    }

    runSetup(): void {
        void this.router.navigate(['/setup']);
    }

    /**
     * Ask both questions, and take the answers only if no later check has started since, so
     * a slow answer cannot bring back what a quicker one has cleared.
     *
     * Each fails on its own account, as a backend without the command rejects the call, and
     * that leaves the prompt hidden instead of leaving the page broken.
     */
    private async check(): Promise<void> {
        const mine = ++this.started;
        const [needed, status] = await Promise.all([
            this.bridge.invoke<boolean>('lumen_setup_needed').catch(() => null),
            this.bridge.invoke<IdentityHookStatus>('lumen_identity_hook_status').catch(() => null),
        ]);
        if (mine !== this.started) return;
        this.setupNeeded.set(typeof needed === 'boolean' ? needed : null);
        this.status.set(asStatus(status));
    }
}

/** The answer if it is a status, else null: nothing is claimed from a reply that is not one. */
function asStatus(v: unknown): IdentityHookStatus | null {
    if (typeof v !== 'object' || v === null) return null;
    const s = v as Partial<IdentityHookStatus>;
    return typeof s.registered === 'boolean' && typeof s.servedElsewhere === 'boolean'
        ? { registered: s.registered, servedElsewhere: s.servedElsewhere }
        : null;
}
