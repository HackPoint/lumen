import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/**
 * Lumen brand mark — a luminous aperture: an open ring (echoing the app's hero
 * gauge and the tray ring) around a glowing core (the "light"). Themed via
 * --lumen-* tokens, so it adapts to dark/light automatically. Size it by
 * setting `font-size` on the host; everything scales in `em`.
 *
 *   <lumen-logo/>                 -> mark + "Lumen" wordmark (default)
 *   <lumen-logo variant="mark"/>  -> mark only
 */
@Component({
    selector: 'lumen-logo',
    imports: [],
    changeDetection: ChangeDetectionStrategy.OnPush,
    template: `
        <span class="logo" [attr.data-variant]="variant()">
            <svg class="logo__mark" viewBox="0 0 32 32" aria-hidden="true" focusable="false">
                <defs>
                    <linearGradient [attr.id]="arcId" x1="0" y1="0" x2="1" y2="1">
                        <stop offset="0" stop-color="var(--lumen-brand-2)" />
                        <stop offset="1" stop-color="var(--lumen-brand)" />
                    </linearGradient>
                    <radialGradient [attr.id]="coreId" cx="50%" cy="50%" r="50%">
                        <stop offset="0" stop-color="var(--lumen-brand-2)" />
                        <stop offset="1" stop-color="var(--lumen-brand)" />
                    </radialGradient>
                    <radialGradient [attr.id]="glowId" cx="50%" cy="50%" r="50%">
                        <stop offset="0" stop-color="var(--lumen-brand)" stop-opacity="0.55" />
                        <stop offset="1" stop-color="var(--lumen-brand)" stop-opacity="0" />
                    </radialGradient>
                </defs>

                <!-- soft luminous halo -->
                <circle class="logo__glow" cx="16" cy="16" r="9" [attr.fill]="'url(#' + glowId + ')'" />

                <!-- aperture ring: open arc, rhyming with the gauge (start 135°, ~280° sweep) -->
                <circle
                    class="logo__ring"
                    cx="16" cy="16" r="11.5"
                    fill="none"
                    [attr.stroke]="'url(#' + arcId + ')'"
                    stroke-width="3.2"
                    stroke-linecap="round"
                    pathLength="100"
                    stroke-dasharray="80 100"
                    transform="rotate(135 16 16)"
                />

                <!-- glowing core -->
                <circle class="logo__core" cx="16" cy="16" r="3.1" [attr.fill]="'url(#' + coreId + ')'" />
            </svg>

            @if (variant() === 'lockup') {
                <span class="logo__word">Lumen</span>
            }
        </span>
    `,
    styles: [
        `
        :host {
            display: inline-flex;
            line-height: 1;
        }

        .logo {
            display: inline-flex;
            align-items: center;
            gap: 0.5em;
            line-height: 1;
        }

        .logo__mark {
            width: 1.15em;
            height: 1.15em;
            display: block;
            flex: 0 0 auto;
            overflow: visible;
            filter: drop-shadow(0 0 1.5px color-mix(in srgb, var(--lumen-brand) 45%, transparent));
        }

        .logo__ring {
            transition: stroke 0.4s ease;
        }

        .logo__word {
            font-family: var(--lumen-font-sans);
            font-weight: 600;
            font-size: 1em;
            letter-spacing: 0.2px;
            color: var(--lumen-text);
        }
        `,
    ],
})
export class Logo {
    readonly variant = input<'lockup' | 'mark'>('lockup');

    // Unique gradient ids so multiple lockups never collide in one document.
    private static seq = 0;
    private readonly uid = `lumen-logo-${Logo.seq++}`;
    readonly arcId = `${this.uid}-arc`;
    readonly coreId = `${this.uid}-core`;
    readonly glowId = `${this.uid}-glow`;
}
