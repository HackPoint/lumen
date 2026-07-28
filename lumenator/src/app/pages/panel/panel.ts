import { ChangeDetectionStrategy, Component, OnInit, computed, inject } from '@angular/core';
import { Cost } from '../../components/cost/cost';
import { Firefly } from '../../components/firefly/firefly';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';

@Component({
    selector: 'panel',
    imports: [Cost, Firefly],
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

    ngOnInit(): void {
        void this.bridge.moveWindowToTray().catch(() => {});
    }
}
