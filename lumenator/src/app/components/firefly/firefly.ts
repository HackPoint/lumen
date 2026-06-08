import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

type FireflyState  = 'full' | 'soft' | 'idle';
type FireflyMode   = 'logo' | 'battery';
type FireflyStatus = 'ok' | 'warn' | 'alert';

/**
 * Lumen firefly mascot — the single brand mark used across all surfaces.
 *
 * Props:
 *   state  — channel-driven glow in logo mode (full=green, soft=amber, idle=dim)
 *   mode   — 'logo': plain brand mark.  'battery': body fills from bottom up.
 *   fill   — 0–100 percent fill for battery mode (drives the lit-body clip).
 *   status — ok/warn/alert color in battery mode (matches gauge status palette).
 *
 * Breathing glow: continuous slow pulse on the tail lantern (~3 s). Frozen when
 * prefers-reduced-motion: reduce is set (accessibility + battery saving).
 *
 * Size: set font-size on the host; all em-based dimensions scale with it.
 * Self-contained — inline template + styles; safe to reuse in docs/README.
 */
@Component({
    selector: 'lumen-firefly',
    imports: [],
    changeDetection: ChangeDetectionStrategy.OnPush,
    template: `
        <span class="firefly"
              [attr.data-state]="state()"
              [attr.data-mode]="mode()"
              [attr.data-status]="status()">
            <svg class="firefly__svg" viewBox="0 0 48 52"
                 aria-hidden="true" focusable="false" overflow="visible">
                <defs>
                    <!-- Halo radial gradient — color tracks glow state/status -->
                    <radialGradient [attr.id]="haloId" cx="50%" cy="80%" r="55%">
                        <stop offset="0" [attr.stop-color]="glowColor()" stop-opacity="0.85"/>
                        <stop offset="1" [attr.stop-color]="glowColor()" stop-opacity="0"/>
                    </radialGradient>

                    <!--
                        Battery fill clip: a rectangle that reveals the lit overlay
                        from fillClipY (which rises as fill increases) downward.
                        At fill=0 → rect starts at bottom of body, covering nothing.
                        At fill=100 → rect starts at top of head, covering everything.
                        In logo mode this clipPath is defined but never referenced.
                    -->
                    <clipPath [attr.id]="fillClipId">
                        <rect x="-2" [attr.y]="fillClipY()" width="52" height="60"/>
                    </clipPath>
                </defs>

                <!-- Ground halo (pulses with the active glow) -->
                <ellipse class="firefly__halo"
                         cx="24" cy="46" rx="13" ry="5.5"
                         [attr.fill]="'url(#' + haloId + ')'"/>

                <!-- Wings: slightly transparent; colors match body state/status -->
                <ellipse class="firefly__wing"
                         cx="13" cy="23" rx="8" ry="4"
                         fill="var(--ff-wing)"
                         transform="rotate(-30 13 23)"/>
                <ellipse class="firefly__wing"
                         cx="35" cy="23" rx="8" ry="4"
                         fill="var(--ff-wing)"
                         transform="rotate(30 35 23)"/>

                <!--
                    Base body + head.
                    Logo mode: colored by --ff-body (state-driven).
                    Battery mode: --ff-body is dim/empty; the lit overlay below
                    provides the fill color.
                -->
                <ellipse cx="24" cy="22" rx="5" ry="8.5" fill="var(--ff-body)"/>
                <circle  cx="24" cy="13" r="4"            fill="var(--ff-body)"/>

                <!--
                    Battery lit overlay: body + head drawn again in --ff-lit,
                    clipped to the fill level (bottom-up). Only present in battery
                    mode so the extra DOM nodes are never added in logo mode.
                -->
                @if (mode() === 'battery') {
                    <ellipse cx="24" cy="22" rx="5" ry="8.5" fill="var(--ff-lit)"
                             [attr.clip-path]="'url(#' + fillClipId + ')'"/>
                    <circle  cx="24" cy="13" r="4"           fill="var(--ff-lit)"
                             [attr.clip-path]="'url(#' + fillClipId + ')'"/>
                }

                <!-- Antennae + tips -->
                <line x1="22" y1="10" x2="16" y2="4"
                      stroke="var(--ff-body)" stroke-width="1.3" stroke-linecap="round"/>
                <line x1="26" y1="10" x2="32" y2="4"
                      stroke="var(--ff-body)" stroke-width="1.3" stroke-linecap="round"/>
                <circle cx="16" cy="4" r="1.2" fill="var(--ff-body)"/>
                <circle cx="32" cy="4" r="1.2" fill="var(--ff-body)"/>

                <!-- Tail lantern — the bioluminescent organ (always lit) -->
                <ellipse class="firefly__tail"
                         cx="24" cy="33" rx="6" ry="5" fill="var(--ff-tail)"/>
                <!-- Bright core: glows + pulses -->
                <ellipse class="firefly__core"
                         cx="24" cy="33" rx="3" ry="2.5" fill="var(--ff-core)"/>
            </svg>
        </span>
    `,
    styles: [`
        :host {
            display: inline-flex;
            align-items: center;
            line-height: 1;
        }

        /* ── Default idle colors ── */
        .firefly {
            --ff-body: var(--lumen-text-dim);
            --ff-wing: color-mix(in srgb, var(--lumen-text-dim) 22%, transparent);
            --ff-tail: color-mix(in srgb, var(--lumen-text-dim) 30%, transparent);
            --ff-core: var(--lumen-text-dim);
            --ff-lit:  var(--lumen-text-dim); /* unused in logo/idle but avoids var warning */
        }

        /* ── Logo mode state-driven colors ── */
        .firefly[data-state="full"] {
            --ff-body: color-mix(in srgb, var(--lumen-ok) 70%, var(--lumen-text));
            --ff-wing: color-mix(in srgb, var(--lumen-ok) 18%, transparent);
            --ff-tail: color-mix(in srgb, var(--lumen-ok) 52%, transparent);
            --ff-core: var(--lumen-ok);
        }
        .firefly[data-state="soft"] {
            --ff-body: color-mix(in srgb, var(--lumen-warn) 70%, var(--lumen-text));
            --ff-wing: color-mix(in srgb, var(--lumen-warn) 18%, transparent);
            --ff-tail: color-mix(in srgb, var(--lumen-warn) 52%, transparent);
            --ff-core: var(--lumen-warn);
        }

        /* ── Battery mode: status sets --ff-status, which drives all colors ── */
        .firefly[data-mode="battery"][data-status="ok"]    { --ff-status: var(--lumen-ok); }
        .firefly[data-mode="battery"][data-status="warn"]  { --ff-status: var(--lumen-warn); }
        .firefly[data-mode="battery"][data-status="alert"] { --ff-status: var(--lumen-alert); }

        .firefly[data-mode="battery"] {
            --ff-body: color-mix(in srgb, var(--lumen-text-dim) 35%, transparent);
            --ff-lit:  var(--ff-status, var(--lumen-ok));
            --ff-wing: color-mix(in srgb, var(--lumen-text-dim) 22%, transparent);
            --ff-tail: color-mix(in srgb, var(--ff-status, var(--lumen-ok)) 52%, transparent);
            --ff-core: var(--ff-status, var(--lumen-ok));
        }

        /* ── SVG sizing ── */
        .firefly__svg {
            width: 1em;
            height: 1.085em;
            display: block;
            overflow: visible;
        }

        /* ── Core glow via drop-shadow ── */
        .firefly__core {
            filter: drop-shadow(0 0 4px var(--ff-core));
        }

        /* ── Pulse animations (logo mode) ── */
        .firefly[data-state="full"] .firefly__tail,
        .firefly[data-state="full"] .firefly__core {
            animation: ff-pulse 2.2s ease-in-out infinite;
        }
        .firefly[data-state="soft"] .firefly__tail,
        .firefly[data-state="soft"] .firefly__core {
            animation: ff-pulse 3.6s ease-in-out infinite;
        }

        /* ── Pulse animations (battery mode — always on; speed by urgency) ── */
        .firefly[data-mode="battery"][data-status="ok"] .firefly__tail,
        .firefly[data-mode="battery"][data-status="ok"] .firefly__core {
            animation: ff-pulse 3.2s ease-in-out infinite;
        }
        .firefly[data-mode="battery"][data-status="warn"] .firefly__tail,
        .firefly[data-mode="battery"][data-status="warn"] .firefly__core {
            animation: ff-pulse 2.4s ease-in-out infinite;
        }
        .firefly[data-mode="battery"][data-status="alert"] .firefly__tail,
        .firefly[data-mode="battery"][data-status="alert"] .firefly__core {
            animation: ff-pulse 1.6s ease-in-out infinite;
        }

        /* ── Ground halo ── */
        .firefly__halo { opacity: 0; }

        .firefly[data-state="full"]  .firefly__halo { animation: ff-halo 2.2s ease-in-out infinite; }
        .firefly[data-state="soft"]  .firefly__halo { animation: ff-halo 3.6s ease-in-out infinite; }

        /* Halo burns progressively hotter ok→warn→alert (distinct keyframes for peak opacity) */
        .firefly[data-mode="battery"][data-status="ok"]    .firefly__halo { animation: ff-halo-ok    3.2s ease-in-out infinite; }
        .firefly[data-mode="battery"][data-status="warn"]  .firefly__halo { animation: ff-halo-warn  2.4s ease-in-out infinite; }
        .firefly[data-mode="battery"][data-status="alert"] .firefly__halo { animation: ff-halo-alert 1.6s ease-in-out infinite; }

        /* Core glow intensifies with urgency */
        .firefly[data-mode="battery"][data-status="warn"]  .firefly__core { filter: drop-shadow(0 0 6px var(--ff-core)); }
        .firefly[data-mode="battery"][data-status="alert"] .firefly__core { filter: drop-shadow(0 0 9px var(--ff-core)); }

        @keyframes ff-pulse {
            0%, 100% { opacity: 0.55; }
            50%       { opacity: 1; }
        }
        /* Logo-mode halo (used by data-state full/soft) */
        @keyframes ff-halo {
            0%, 100% { opacity: 0.18; }
            50%       { opacity: 0.62; }
        }
        /* Battery-mode halo — peaks rise with urgency */
        @keyframes ff-halo-ok    { 0%, 100% { opacity: 0.14; } 50% { opacity: 0.52; } }
        @keyframes ff-halo-warn  { 0%, 100% { opacity: 0.20; } 50% { opacity: 0.72; } }
        @keyframes ff-halo-alert { 0%, 100% { opacity: 0.28; } 50% { opacity: 0.90; } }

        /* ── Accessibility: freeze all animation ── */
        @media (prefers-reduced-motion: reduce) {
            .firefly__tail,
            .firefly__core,
            .firefly__halo { animation: none !important; }
        }
    `],
})
export class Firefly {
    readonly state  = input<FireflyState>('idle');
    readonly mode   = input<FireflyMode>('logo');
    readonly fill   = input<number>(0);
    readonly status = input<FireflyStatus>('ok');

    // ── Glow gradient color (parameterizes the SVG radialGradient) ──────────

    readonly glowColor = computed((): string => {
        if (this.mode() === 'battery') {
            switch (this.status()) {
                case 'alert': return 'var(--lumen-alert)';
                case 'warn':  return 'var(--lumen-warn)';
                default:      return 'var(--lumen-ok)';
            }
        }
        switch (this.state()) {
            case 'full': return 'var(--lumen-ok)';
            case 'soft': return 'var(--lumen-warn)';
            default:     return 'var(--lumen-text-dim)';
        }
    });

    // ── Battery fill clip Y position ─────────────────────────────────────────
    //
    // The clip rect starts at fillClipY and extends downward to y=60 (below the
    // canvas).  fillClipY moves UP as fill% increases, mapping DIRECTLY to body%:
    //   fill=  0 → y=30.5 (bottom of body — lit overlay covers nothing)
    //   fill= 50 → y=22.0 (body center — bottom half of body lit)
    //   fill= 71 → y=18.43 (71% of body height lit)
    //   fill=100 → y=13.5  (entire body lit; head starts lighting above ~80%)
    //
    // Range is body-only (bot=30.5, top=13.5) so fill% == body-lit%.
    // The lit head circle is also clipped: it becomes visible only above ~80%
    // fill when fillClipY crosses the head's bottom edge (y=17), giving a
    // natural urgency signal as the context approaches warn/alert thresholds.

    readonly fillClipY = computed((): number => {
        const bot = 30.5; // bottom of body ellipse (cy=22, ry=8.5)
        const top = 13.5; // top of body ellipse  (cy=22, ry=8.5)
        return bot - (Math.max(0, Math.min(100, this.fill())) / 100) * (bot - top);
    });

    // ── Unique gradient/clip IDs per instance ────────────────────────────────
    private static seq = 0;
    private readonly uid = `ff-${Firefly.seq++}`;
    readonly haloId    = `${this.uid}-halo`;
    readonly fillClipId = `${this.uid}-fill`;
}
