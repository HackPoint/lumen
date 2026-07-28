import { Component, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { LumenTooltip } from './tooltip.directive';

@Component({
  imports: [LumenTooltip],
  template: `<button type="button" [lumenTooltip]="text()">info</button>`,
})
class Host {
  readonly text = signal('Context fill is the cache-read token count.');
}

/**
 * The tooltip appends a fixed-position node to document.body so it escapes
 * overflow:hidden ancestors. That means it must clean up after itself, and it
 * must stay reachable for keyboard and screen-reader users.
 */
describe('LumenTooltip', () => {
  let fixture: ComponentFixture<Host>;
  let button: HTMLButtonElement;

  beforeEach(() => {
    fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    button = (fixture.nativeElement as HTMLElement).querySelector('button')!;
  });

  afterEach(() => {
    fixture.destroy();
    // Nothing may be left behind on body between cases.
    expect(document.querySelectorAll('.lumen-tooltip')).toHaveLength(0);
  });

  function tip(): HTMLElement | null {
    return document.querySelector('.lumen-tooltip');
  }

  function isVisible(): boolean {
    return tip()?.classList.contains('lumen-tooltip--visible') ?? false;
  }

  // ── accessibility ──────────────────────────────────────────────────────────

  it('mirrors the tooltip text onto aria-label so it is announced on focus', () => {
    expect(button.getAttribute('aria-label')).toBe(
      'Context fill is the cache-read token count.',
    );
  });

  it('updates aria-label when the text changes', () => {
    fixture.componentInstance.text.set('Updated description.');
    fixture.detectChanges();
    expect(button.getAttribute('aria-label')).toBe('Updated description.');
  });

  it('marks the tooltip node with role=tooltip', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(tip()?.getAttribute('role')).toBe('tooltip');
  });

  // ── show / hide ────────────────────────────────────────────────────────────

  it('creates nothing until first shown', () => {
    expect(tip()).toBeNull();
  });

  it('shows on hover and hides on leave', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(isVisible()).toBe(true);
    expect(tip()?.textContent).toBe('Context fill is the cache-read token count.');

    button.dispatchEvent(new MouseEvent('mouseleave'));
    expect(isVisible()).toBe(false);
  });

  it('shows on focus and hides on blur, for keyboard users', () => {
    button.dispatchEvent(new FocusEvent('focus'));
    expect(isVisible()).toBe(true);
    button.dispatchEvent(new FocusEvent('blur'));
    expect(isVisible()).toBe(false);
  });

  it('toggles on click, so a trackpad tap works in the tray panel', () => {
    button.dispatchEvent(new MouseEvent('click'));
    expect(isVisible()).toBe(true);
    button.dispatchEvent(new MouseEvent('click'));
    expect(isVisible()).toBe(false);
  });

  it('dismisses on Escape', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(isVisible()).toBe(true);
    button.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    expect(isVisible()).toBe(false);
  });

  it('reuses one node across repeated shows instead of leaking one each time', () => {
    for (let i = 0; i < 5; i++) {
      button.dispatchEvent(new MouseEvent('mouseenter'));
      button.dispatchEvent(new MouseEvent('mouseleave'));
    }
    expect(document.querySelectorAll('.lumen-tooltip')).toHaveLength(1);
  });

  it('picks up changed text on the next show', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    button.dispatchEvent(new MouseEvent('mouseleave'));
    fixture.componentInstance.text.set('Second description.');
    fixture.detectChanges();
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(tip()?.textContent).toBe('Second description.');
  });

  // ── positioning ────────────────────────────────────────────────────────────

  it('positions itself with fixed coordinates', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    const t = tip()!;
    expect(t.style.top).toMatch(/px$/);
    expect(t.style.left).toMatch(/px$/);
  });

  it('never places itself off the left edge of the viewport', () => {
    // Regression: the right-edge clamp used to run last and could undo the
    // left-edge clamp, yielding a negative offset whenever the tooltip was wider
    // than the viewport — reachable in the narrow tray popover. jsdom's
    // zero-size rects and zero clientWidth are exactly that degenerate case.
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(parseFloat(tip()!.style.left)).toBeGreaterThanOrEqual(0);
  });

  // ── cleanup ────────────────────────────────────────────────────────────────

  it('removes its node from the body on destroy', () => {
    button.dispatchEvent(new MouseEvent('mouseenter'));
    expect(tip()).not.toBeNull();
    fixture.destroy();
    expect(tip()).toBeNull();
  });
});
