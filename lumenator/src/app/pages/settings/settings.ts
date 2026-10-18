import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { Cost } from '../../components/cost/cost';
import { Usage } from '../../components/usage/usage';
import { Firefly } from '../../components/firefly/firefly';
import { TabNav } from '../../components/tab-nav/tab-nav';
import { SessionService } from '../../session.service';
import { RepeatReadsService } from '../../repeat-reads.service';
import { BILLING_CHOICES, BillingMode, BillingModeService } from '../../billing-mode.service';
import { TauriBridge } from '../../tauri-bridge';
import { LumenTooltip } from '../../directives/tooltip.directive';

/**
 * Settings: what the user can change, the spend figures the alert thresholds are set against,
 * fault reporting and the version. Lumen had no settings screen before 1.9.0; these were spread
 * over Home and Hotspots.
 *
 * The spend figures sit with the thresholds because a threshold means nothing without the
 * figure it is compared with. They are Claude Code's spend, not Lumen's savings, which is why
 * they left Home.
 */
@Component({
    selector: 'settings',
    imports: [Cost, Usage, Firefly, TabNav, LumenTooltip],
    templateUrl: './settings.html',
    styleUrl: './settings.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Settings implements OnInit {
    readonly s = inject(SessionService);
    readonly rr = inject(RepeatReadsService);
    readonly billing = inject(BillingModeService);
    private readonly bridge = inject(TauriBridge);

    readonly billingChoices = BILLING_CHOICES;

    /**
     * The version this build was made as, as the Tauri runtime reports it, or null until it has.
     * Read from the runtime, never written here; see Home's header, which shows the same value.
     */
    readonly version = signal<string | null>(null);

    /** Set when the runtime could not say, so the About section says so rather than staying blank. */
    readonly versionFailed = signal(false);

    /** The About section's value: the version, `unavailable` once reading it has failed, else empty. */
    readonly aboutVersion = computed(() => this.version() ?? (this.versionFailed() ? 'unavailable' : ''));

    ngOnInit(): void {
        void this.rr.load();
        void this.billing.load();
        this.bridge.getVersion()
            .then(v => this.version.set(v))
            // Non-fatal, and said so in the About section instead of faking a number.
            .catch(() => this.versionFailed.set(true));
    }

    // ── billing mode ──────────────────────────────────────────────────────────

    async onBillingMode(e: Event, mode: BillingMode): Promise<void> {
        const radio = e.target as HTMLInputElement;
        if (this.billing.saving()) {
            this.putBillingBack(radio);
            return;
        }
        const took = await this.billing.set(mode);
        // A click checks the radio by itself, and the binding writes it back only when the
        // stored value changed, so a choice that did not take has to be put back here.
        if (!took) this.putBillingBack(radio);
    }

    /** Check the radio the file says, and only it: checking one radio unchecked its sibling. */
    private putBillingBack(radio: HTMLInputElement): void {
        const mode = this.billing.setting()?.mode;
        const group = radio.closest('[role="radiogroup"]')?.querySelectorAll<HTMLInputElement>('input[type="radio"]');
        for (const r of Array.from(group ?? [radio])) r.checked = r.value === mode;
    }

    // ── alert thresholds (in memory, reset on restart) ────────────────────────

    onDailyLimit(e: Event): void {
        const v = +(e.target as HTMLInputElement).value;
        if (v >= 0) this.s.setDailyLimit(v);
    }

    onSessionLimit(e: Event): void {
        const v = +(e.target as HTMLInputElement).value;
        if (v >= 0) this.s.setSessionLimit(v);
    }

    onNativeNotify(e: Event): void {
        this.s.setNativeNotify((e.target as HTMLInputElement).checked);
    }

    // ── repeat reads ──────────────────────────────────────────────────────────

    async onRepeatReads(e: Event): Promise<void> {
        const box = e.target as HTMLInputElement;
        // Left enabled while it writes: a disabled box would drop the keyboard focus it holds.
        if (this.rr.saving()) {
            box.checked = !box.checked;
            return;
        }
        const took = await this.rr.set(box.checked);
        // A click flips the box by itself, and the binding writes it back only when the
        // stored value changed, so a write that did not take has to put it back here.
        const now = this.rr.setting();
        if (!took && now) box.checked = now.on;
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
