import {
    ChangeDetectionStrategy,
    Component,
    OnInit,
    signal,
} from '@angular/core';
import { invoke } from '@tauri-apps/api/core';
import { Router } from '@angular/router';
import { Firefly } from '../../components/firefly/firefly';

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

    private readonly router: Router;

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
            const result = await invoke<SetupStep[]>('lumen_run_setup');
            this.steps.set(result);
            this.phase.set('done');
        } catch (e: unknown) {
            this.error.set(String(e));
            this.phase.set('done');
        }
    }

    async uninstall(): Promise<void> {
        this.phase.set('running');
        this.steps.set([]);
        this.error.set(null);
        try {
            const result = await invoke<SetupStep[]>('lumen_uninstall');
            this.steps.set(result);
            this.phase.set('done');
        } catch (e: unknown) {
            this.error.set(String(e));
            this.phase.set('done');
        }
    }

    goHome(): void {
        this.router.navigate(['/']);
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
