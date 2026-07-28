import {
    ChangeDetectionStrategy,
    Component,
    OnInit,
    inject,
    signal,
} from '@angular/core';
import { Router } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';
import { TauriBridge } from '../../tauri-bridge';

type StepStatus = 'Ok' | 'Warn' | 'Error' | 'Skip';

interface SetupStep {
    id:     string;
    label:  string;
    status: StepStatus;
    detail: string;
}

type Phase = 'idle' | 'running' | 'done';

@Component({
    selector: 'lumen-setup',
    imports: [Firefly],
    templateUrl: './setup.html',
    styleUrl: './setup.css',
    changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Setup implements OnInit {
    readonly phase  = signal<Phase>('idle');
    readonly steps  = signal<SetupStep[]>([]);
    readonly error  = signal<string | null>(null);

    readonly cliRunning = signal(false);
    readonly cliResult  = signal<string | null>(null);

    /**
     * Whether Lumen is registered to start at login.
     *
     * Setup turns this on, but it stays a toggle: a login item is the user's
     * machine, not ours, and someone who turns it off must not have it silently
     * re-enabled behind their back.
     */
    readonly autostart        = signal(false);
    readonly autostartBusy    = signal(false);
    readonly autostartError   = signal<string | null>(null);

    private readonly router: Router;
    private readonly bridge = inject(TauriBridge);

    constructor(router: Router) {
        this.router = router;
    }

    ngOnInit(): void {
        this.runSetup();
    }

    async runSetup(): Promise<void> {
        this.phase.set('running');
        this.steps.set([]);
        this.error.set(null);
        try {
            const result = await this.bridge.invoke<SetupStep[]>('lumen_run_setup');
            this.steps.set(result);
            this.phase.set('done');
        } catch (e: unknown) {
            this.error.set(String(e));
            this.phase.set('done');
        }
        // Read the real state rather than assuming setup's step succeeded — it
        // reports Warn, not Error, when the login item could not be registered.
        await this.refreshAutostart();
    }

    /** Read the current login-item state from the backend. */
    async refreshAutostart(): Promise<void> {
        try {
            this.autostart.set(await this.bridge.invoke<boolean>('lumen_autostart_enabled'));
        } catch {
            // A backend that cannot answer is reported as off; the toggle still
            // renders and the user can try to switch it on.
            this.autostart.set(false);
        }
    }

    async toggleAutostart(): Promise<void> {
        this.autostartBusy.set(true);
        this.autostartError.set(null);
        const want = !this.autostart();
        try {
            // Trust the returned state over `want`: the OS is the authority on
            // whether the login item actually exists now.
            this.autostart.set(
                await this.bridge.invoke<boolean>('lumen_set_autostart', { enable: want }),
            );
        } catch (e: unknown) {
            this.autostartError.set(String(e));
            await this.refreshAutostart();
        } finally {
            this.autostartBusy.set(false);
        }
    }

    async uninstall(): Promise<void> {
        this.phase.set('running');
        this.steps.set([]);
        this.error.set(null);
        try {
            const result = await this.bridge.invoke<SetupStep[]>('lumen_uninstall');
            this.steps.set(result);
            this.phase.set('done');
        } catch (e: unknown) {
            this.error.set(String(e));
            this.phase.set('done');
        }
        // Uninstall removes the login item, so the toggle must not keep claiming
        // it is on.
        await this.refreshAutostart();
    }

    goHome(): void {
        this.router.navigate(['/']);
    }

    async installCli(): Promise<void> {
        this.cliRunning.set(true);
        this.cliResult.set(null);
        try {
            const steps = await this.bridge.invoke<SetupStep[]>('lumen_install_cli');
            const step = steps[0];
            if (step) {
                const icon = step.status === 'Ok' ? '✓' : '✕';
                this.cliResult.set(`${icon} ${step.detail}`);
            }
        } catch (e: unknown) {
            this.cliResult.set(`✕ ${String(e)}`);
        } finally {
            this.cliRunning.set(false);
        }
    }

    get allOk(): boolean {
        return this.steps().every(s => s.status === 'Ok' || s.status === 'Warn');
    }

    get hasError(): boolean {
        return this.steps().some(s => s.status === 'Error');
    }

    iconFor(status: StepStatus): string {
        switch (status) {
            case 'Ok':   return '✓';
            case 'Warn': return '!';
            case 'Error': return '✕';
            case 'Skip': return '–';
        }
    }

    fireflyState(): 'full' | 'soft' | 'idle' {
        if (this.phase() === 'running') return 'full';
        if (this.hasError)              return 'soft';
        return 'full';
    }
}
