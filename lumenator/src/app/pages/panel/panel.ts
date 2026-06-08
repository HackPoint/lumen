import { ChangeDetectionStrategy, Component, OnInit, computed, inject } from '@angular/core';
import { Cost } from '../../components/cost/cost';
import { Firefly } from '../../components/firefly/firefly';
import { SessionService } from '../../session.service';
import { moveWindow, Position } from '@tauri-apps/plugin-positioner';

@Component({
    selector: 'panel',
    imports: [Cost, Firefly],
    templateUrl: './panel.html',
    styleUrl: './panel.css',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class Panel implements OnInit {
    readonly s = inject(SessionService);

    readonly fmtFill = computed(() => this.s.fill().toLocaleString());
    readonly fmtMax  = computed(() => this.s.maxContext().toLocaleString());
    readonly fillPct = computed(() => this.s.trayPercent());

    ngOnInit(): void {
        void moveWindow(Position.TrayBottomCenter).catch(() => {});
    }
}
