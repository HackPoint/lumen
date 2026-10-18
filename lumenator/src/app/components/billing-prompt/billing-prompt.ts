import { ChangeDetectionStrategy, Component, ElementRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { BILLING_CHOICES, BillingMode, BillingModeService } from '../../billing-mode.service';

/**
 * Asks once how the user pays for Claude Code, on Home, after Setup.
 *
 * The answer decides what a saving is worth to them, and only they know it, so it is asked
 * rather than guessed. It shows while the settings file holds no answer and nothing is wrong
 * with the file, and goes once one is chosen; Settings changes it after that.
 *
 * Hosted by Home only, and only once Setup is known to be done: never on the Setup screen,
 * where it would compete with the step that has to happen first, nor in the tray popover. A
 * file it cannot read keeps it hidden: Settings says what is wrong, and asking again would
 * talk over that.
 */
@Component({
    selector: 'lumen-billing-prompt',
    templateUrl: './billing-prompt.html',
    styleUrl: './billing-prompt.scss',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class BillingPrompt implements OnInit {
    readonly billing = inject(BillingModeService);
    readonly choices = BILLING_CHOICES;

    /** Set once a choice was made here, so a failed write keeps the prompt up to say why. */
    private readonly chose = signal(false);

    /**
     * What was saved, said in a polite live region once a choice takes.
     *
     * The region is in the page from the start and only its text changes: one that appears with
     * its text already in it is not reliably announced. Empty until then, and after a choice
     * that did not take, which the prompt's own alert says.
     */
    readonly confirmation = signal('');

    private readonly status = viewChild.required<ElementRef<HTMLElement>>('status');

    readonly visible = computed(
        () => this.billing.unasked() || (this.chose() && this.billing.writeError() !== null),
    );

    ngOnInit(): void {
        void this.billing.load();
    }

    async choose(mode: BillingMode): Promise<void> {
        if (this.billing.saving()) return;
        this.chose.set(true);
        if (!(await this.billing.set(mode))) return;
        const label = this.choices.find((c) => c.mode === mode)?.label ?? mode;
        this.confirmation.set(`Billing mode saved: ${label}. You can change it in Settings.`);
        // The button that had focus goes with the prompt, and focus would drop to the page body.
        // The sentence saying what was saved is where the prompt was, so it takes focus.
        this.status().nativeElement.focus();
    }
}
