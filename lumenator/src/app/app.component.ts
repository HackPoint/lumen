import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterOutlet } from '@angular/router';

/**
 * Thin shell. Its only job is to host the router outlet — the actual content
 * (Home vs Panel) is chosen by the URL each window loads ("/" vs "/panel").
 * It injects nothing and renders no content itself, so it never double-renders
 * a routed component.
 */
@Component({
    selector: 'app-root',
    imports: [RouterOutlet],
    templateUrl: './app.component.html',
    styleUrl: './app.component.css',
    changeDetection: ChangeDetectionStrategy.OnPush
})
export class AppComponent {}
