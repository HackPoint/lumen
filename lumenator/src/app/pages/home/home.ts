import { ChangeDetectionStrategy, Component, OnInit, computed, inject } from '@angular/core';
import { RouterLink, RouterLinkActive, Router } from '@angular/router';
import { Gauge } from '../../components/gauge/gauge';
import { Cost } from '../../components/cost/cost';
import { Usage } from '../../components/usage/usage';
import { Firefly } from '../../components/firefly/firefly';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';

@Component({
    selector: 'home',
    imports: [RouterLink, RouterLinkActive, Gauge, Cost, Usage, Firefly],
    templateUrl: './home.html',
    styleUrl: './home.css',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Home implements OnInit {
    readonly s = inject(SessionService);
    private readonly router = inject(Router);
    private readonly bridge = inject(TauriBridge);

    ngOnInit(): void {
        this.bridge.invoke<boolean>('lumen_setup_needed')
            .then(needed => { if (needed) this.router.navigate(['/setup']); })
            .catch(() => { /* non-fatal: proceed normally if command fails */ });
    }

    readonly activeIndex = computed(() => {
        const i = this.s.windowOptions.findIndex((o) => o.value === this.s.contextOverride());
        return i < 0 ? 0 : i;
    });

    // D4: unwrap number inputs for threshold settings.
    onDailyLimit(e: Event): void {
        const v = +(e.target as HTMLInputElement).value;
        if (v >= 0) this.s.setDailyLimit(v);
    }

    onSessionLimit(e: Event): void {
        const v = +(e.target as HTMLInputElement).value;
        if (v >= 0) this.s.setSessionLimit(v);
    }

    // D5: toggle native OS notifications.
    onNativeNotify(e: Event): void {
        this.s.setNativeNotify((e.target as HTMLInputElement).checked);
    }
}
