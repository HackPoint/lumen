import {
    Directive,
    ElementRef,
    HostListener,
    OnDestroy,
    Renderer2,
    inject,
    input,
} from '@angular/core';
import { DOCUMENT } from '@angular/common';

/**
 * Tooltip shown on hover, focus, and click-toggle. Appends a fixed-position
 * div to document.body so it is never clipped by overflow:hidden ancestors
 * (e.g. the tray panel or the gauge meta area).
 *
 * The directive also sets aria-label on the host to the tooltip text so
 * keyboard / screen-reader users hear the full description on focus without
 * needing to activate the tooltip.
 *
 * Usage:
 *   <button class="info-btn" type="button" [lumenTooltip]="'Tooltip text.'">ⓘ</button>
 */
@Directive({
    selector: '[lumenTooltip]',
    standalone: true,
    host: {
        '[attr.aria-label]': 'lumenTooltip()',
    },
})
export class LumenTooltip implements OnDestroy {
    readonly lumenTooltip = input.required<string>();

    private readonly el = inject<ElementRef<HTMLElement>>(ElementRef);
    private readonly renderer = inject(Renderer2);
    private readonly doc = inject(DOCUMENT);

    private tip: HTMLElement | null = null;
    private open = false;

    @HostListener('mouseenter') onEnter(): void { this.show(); }
    @HostListener('mouseleave') onLeave(): void { this.hide(); }
    @HostListener('focus') onFocus(): void { this.show(); }
    @HostListener('blur') onBlur(): void { this.hide(); }
    // Toggle on click so tap on the tray panel (trackpad) works.
    @HostListener('click') onClick(): void { this.open ? this.hide() : this.show(); }
    @HostListener('keydown.escape') onEscape(): void { this.hide(); }

    private show(): void {
        if (!this.tip) {
            this.tip = this.renderer.createElement('div') as HTMLElement;
            this.renderer.addClass(this.tip, 'lumen-tooltip');
            this.renderer.setAttribute(this.tip, 'role', 'tooltip');
            this.renderer.appendChild(this.doc.body, this.tip);
        }
        this.tip.textContent = this.lumenTooltip();
        this.position();
        this.renderer.addClass(this.tip, 'lumen-tooltip--visible');
        this.open = true;
    }

    private hide(): void {
        if (this.tip) {
            this.renderer.removeClass(this.tip, 'lumen-tooltip--visible');
        }
        this.open = false;
    }

    private position(): void {
        const tip = this.tip;
        if (!tip) return;
        const rect = this.el.nativeElement.getBoundingClientRect();
        const GAP = 8;

        // Measure off-screen so we know the tooltip dimensions before placing it.
        tip.style.cssText = 'position:fixed;visibility:hidden;top:0;left:0';
        const tw = tip.offsetWidth;
        const th = tip.offsetHeight;
        tip.style.cssText = '';

        // Prefer above the trigger; flip below if there is not enough room.
        let top = rect.top - th - GAP;
        if (top < GAP) top = rect.bottom + GAP;

        // Center horizontally over the trigger, clamped to the viewport.
        const vw = this.doc.documentElement.clientWidth;
        let left = rect.left + rect.width / 2 - tw / 2;
        if (left < GAP) left = GAP;
        if (left + tw > vw - GAP) left = vw - tw - GAP;

        tip.style.top = `${top}px`;
        tip.style.left = `${left}px`;
    }

    ngOnDestroy(): void {
        if (this.tip) {
            this.renderer.removeChild(this.doc.body, this.tip);
        }
    }
}
