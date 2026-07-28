import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Gauge } from './gauge';

/**
 * The gauge is the app's primary signal. Its job is to be honest about how full
 * the context is — including when the numbers it is handed are nonsense.
 */
describe('Gauge', () => {
  let fixture: ComponentFixture<Gauge>;
  let gauge: Gauge;

  function build(fill: number, max = 200_000, model = ''): Gauge {
    fixture = TestBed.createComponent(Gauge);
    fixture.componentRef.setInput('fill', fill);
    fixture.componentRef.setInput('max', max);
    fixture.componentRef.setInput('model', model);
    fixture.detectChanges();
    gauge = fixture.componentInstance;
    return gauge;
  }

  function html(): string {
    return (fixture.nativeElement as HTMLElement).innerHTML;
  }

  it('is created with sane defaults', () => {
    fixture = TestBed.createComponent(Gauge);
    fixture.detectChanges();
    expect(fixture.componentInstance.fill()).toBe(0);
    expect(fixture.componentInstance.max()).toBe(200_000);
  });

  // ── ratio / percent ────────────────────────────────────────────────────────

  it('computes the ratio and percent from fill over max', () => {
    const g = build(50_000);
    expect(g.ratio()).toBeCloseTo(0.25, 10);
    expect(g.percent()).toBe(25);
  });

  it('clamps an over-full gauge to 100%', () => {
    const g = build(600_000, 200_000);
    expect(g.ratio()).toBe(1);
    expect(g.percent()).toBe(100);
  });

  it('clamps a negative fill to 0%', () => {
    // The data layer passes fill through unclamped, so the clamp has to be here.
    const g = build(-5_000, 200_000);
    expect(g.ratio()).toBe(0);
    expect(g.percent()).toBe(0);
  });

  it('reads an unknown window as empty, not as full', () => {
    // fill/0 is Infinity, which clamped to a confident 100%. An unknown window
    // means we know nothing, so it must read 0 — the same choice the TUI makes.
    const g = build(1_000, 0);
    expect(g.ratio()).toBe(0);
    expect(g.percent()).toBe(0);
  });

  it('never renders NaN% for a zero window and zero fill', () => {
    // 0/0 is NaN, and Math.min/max propagate it all the way to the DOM.
    const g = build(0, 0);
    expect(Number.isNaN(g.ratio())).toBe(false);
    expect(Number.isNaN(g.percent())).toBe(false);
    expect(html()).not.toContain('NaN');
  });

  it('rounds the percent to the nearest whole number', () => {
    expect(build(1_234, 200_000).percent()).toBe(1);
    expect(build(199_000, 200_000).percent()).toBe(100);
  });

  // ── status thresholds ──────────────────────────────────────────────────────

  it.each([
    [0, 'ok'],
    [100_000, 'ok'],
    [159_999, 'ok'],
    [160_000, 'warn'], // exactly 80%
    [189_999, 'warn'],
    [190_000, 'alert'], // exactly 95%
    [200_000, 'alert'],
    [999_999, 'alert'],
  ])('maps a fill of %i to status %s', (fill, expected) => {
    expect(build(fill, 200_000).status()).toBe(expected);
  });

  it('maps each status to its own CSS colour token', () => {
    expect(build(0).fillColor()).toBe('var(--lumen-ok)');
    expect(build(170_000).fillColor()).toBe('var(--lumen-warn)');
    expect(build(195_000).fillColor()).toBe('var(--lumen-alert)');
  });

  // ── arc geometry ───────────────────────────────────────────────────────────

  it('normalises the 270-degree arc to 75 path units', () => {
    const g = build(0);
    expect(g.band).toBe(75);
    expect(g.sweep).toBe(270);
    expect(g.startAngle).toBe(135);
  });

  it('reveals the arc by shrinking the dash offset as fill grows', () => {
    expect(build(0).dashOffset()).toBe(75); // fully hidden
    expect(build(100_000).dashOffset()).toBeCloseTo(37.5, 10); // half
    expect(build(200_000).dashOffset()).toBe(0); // fully revealed
  });

  it('places the compaction marker on the arc at 95%', () => {
    const g = build(0);
    expect(g.compRatio).toBe(0.95);
    // The marker must land within the arc's radius of the centre.
    const dx = g.markerX() - g.cx;
    const dy = g.markerY() - g.cy;
    expect(Math.sqrt(dx * dx + dy * dy)).toBeCloseTo(g.r, 6);
  });

  it('keeps the marker fixed regardless of current fill', () => {
    const low = build(0);
    const [x, y] = [low.markerX(), low.markerY()];
    const high = build(199_000);
    expect(high.markerX()).toBeCloseTo(x, 10);
    expect(high.markerY()).toBeCloseTo(y, 10);
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders the fill and max with thousands separators', () => {
    const g = build(50_000, 200_000);
    expect(g.fmtFill()).toBe((50_000).toLocaleString());
    expect(g.fmtMax()).toBe((200_000).toLocaleString());
    expect(html()).toContain(g.fmtFill());
  });

  it('renders the percentage into the DOM', () => {
    build(50_000);
    expect(html()).toContain('25');
  });

  it('renders the model name when given one', () => {
    build(1_000, 200_000, 'claude-sonnet-4');
    expect(html()).toContain('claude-sonnet-4');
  });

  it('updates the DOM when the fill input changes', () => {
    build(10_000);
    expect(html()).toContain('5');
    fixture.componentRef.setInput('fill', 190_000);
    fixture.detectChanges();
    expect(html()).toContain('95');
  });
});
