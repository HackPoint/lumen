import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Firefly } from './firefly';

/**
 * The firefly is the brand mark and, in battery mode, a second read on context
 * fill. Its two pieces of real logic are the glow colour (which differs by mode)
 * and the clip geometry that makes fill% map onto body-lit%.
 */
describe('Firefly', () => {
  let fixture: ComponentFixture<Firefly>;

  function build(inputs: Partial<Record<string, unknown>> = {}): Firefly {
    fixture = TestBed.createComponent(Firefly);
    for (const [k, v] of Object.entries(inputs)) {
      fixture.componentRef.setInput(k, v);
    }
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function svg(): string {
    return (fixture.nativeElement as HTMLElement).innerHTML;
  }

  it('renders with defaults', () => {
    const f = build();
    expect(f.mode()).toBe('logo');
    // 'idle' (dim) is the right default: until a channel is known we should not
    // imply the optimizer is active.
    expect(f.state()).toBe('idle');
    expect(f.status()).toBe('ok');
    expect(f.fill()).toBe(0);
  });

  // ── glow colour ────────────────────────────────────────────────────────────

  it('takes its glow from the channel state in logo mode', () => {
    expect(build({ mode: 'logo', state: 'full' }).glowColor()).toBe('var(--lumen-ok)');
    expect(build({ mode: 'logo', state: 'soft' }).glowColor()).toBe('var(--lumen-warn)');
    expect(build({ mode: 'logo', state: 'idle' }).glowColor()).toBe('var(--lumen-text-dim)');
  });

  it('takes its glow from the fill status in battery mode', () => {
    expect(build({ mode: 'battery', status: 'ok' }).glowColor()).toBe('var(--lumen-ok)');
    expect(build({ mode: 'battery', status: 'warn' }).glowColor()).toBe('var(--lumen-warn)');
    expect(build({ mode: 'battery', status: 'alert' }).glowColor()).toBe('var(--lumen-alert)');
  });

  it('ignores state in battery mode and status in logo mode', () => {
    // Battery mode is driven by status, so a conflicting state must not win.
    expect(build({ mode: 'battery', status: 'alert', state: 'full' }).glowColor()).toBe(
      'var(--lumen-alert)',
    );
    // And logo mode is driven by state, so a conflicting status must not win.
    expect(build({ mode: 'logo', state: 'idle', status: 'alert' }).glowColor()).toBe(
      'var(--lumen-text-dim)',
    );
  });

  // ── battery clip geometry ──────────────────────────────────────────────────

  it('maps an empty battery to the bottom of the body', () => {
    expect(build({ mode: 'battery', fill: 0 }).fillClipY()).toBeCloseTo(30.5, 10);
  });

  it('maps a full battery to the top of the body', () => {
    expect(build({ mode: 'battery', fill: 100 }).fillClipY()).toBeCloseTo(13.5, 10);
  });

  it('maps half fill to the body centre so fill% equals body-lit%', () => {
    expect(build({ mode: 'battery', fill: 50 }).fillClipY()).toBeCloseTo(22.0, 10);
  });

  it('rises monotonically as fill increases', () => {
    let previous = Infinity;
    for (const fill of [0, 10, 25, 50, 75, 90, 100]) {
      const y = build({ mode: 'battery', fill }).fillClipY();
      expect(y).toBeLessThan(previous);
      previous = y;
    }
  });

  it('clamps fill outside 0-100 instead of drawing off-canvas', () => {
    expect(build({ mode: 'battery', fill: -50 }).fillClipY()).toBeCloseTo(30.5, 10);
    expect(build({ mode: 'battery', fill: 400 }).fillClipY()).toBeCloseTo(13.5, 10);
  });

  it('lights the head only in the top fifth of the range', () => {
    // The lit head becomes visible once the clip crosses the head's bottom edge
    // at y=17 — the documented urgency signal near warn/alert.
    expect(build({ mode: 'battery', fill: 70 }).fillClipY()).toBeGreaterThan(17);
    expect(build({ mode: 'battery', fill: 85 }).fillClipY()).toBeLessThan(17);
  });

  // ── instance isolation ─────────────────────────────────────────────────────

  it('gives each instance unique gradient and clip ids', () => {
    // Two fireflies on one page must not share SVG def ids, or the second would
    // inherit the first's gradient.
    const a = build();
    const first = { halo: a.haloId, clip: a.fillClipId };
    const b = build();
    expect(b.haloId).not.toBe(first.halo);
    expect(b.fillClipId).not.toBe(first.clip);
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('exposes mode, state and status as data attributes for CSS', () => {
    build({ mode: 'battery', state: 'soft', status: 'warn' });
    const host = (fixture.nativeElement as HTMLElement).querySelector('.firefly');
    expect(host?.getAttribute('data-mode')).toBe('battery');
    expect(host?.getAttribute('data-state')).toBe('soft');
    expect(host?.getAttribute('data-status')).toBe('warn');
  });

  it('is decorative, not announced to screen readers', () => {
    build();
    expect(svg()).toContain('aria-hidden="true"');
  });

  it('references its own clip id in the rendered markup', () => {
    const f = build({ mode: 'battery', fill: 50 });
    expect(svg()).toContain(f.haloId);
  });
});
