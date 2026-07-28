import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Cost } from './cost';
import { RATE } from '../index';
import type { CostTotals } from '../index';

/**
 * The cost panel is the one place a wrong number costs the user money in
 * confidence. Every figure derives from the single RATE table.
 */
describe('Cost', () => {
  let fixture: ComponentFixture<Cost>;

  function build(over: Partial<CostTotals> = {}): Cost {
    const totals: CostTotals = {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      ...over,
    };
    fixture = TestBed.createComponent(Cost);
    fixture.componentRef.setInput('totals', totals);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  // ── per-component pricing ──────────────────────────────────────────────────

  it('is all zeros for an empty session', () => {
    const c = build();
    expect(c.costInput()).toBe(0);
    expect(c.costOutput()).toBe(0);
    expect(c.costCacheRead()).toBe(0);
    expect(c.costCacheWrite()).toBe(0);
    expect(c.totalCost()).toBe(0);
  });

  it('prices each token class at its own rate', () => {
    const c = build({ input: 1_000_000, output: 1_000_000, cacheRead: 1_000_000, cacheWrite: 1_000_000 });
    expect(c.costInput()).toBeCloseTo(5.0, 10);
    expect(c.costOutput()).toBeCloseTo(25.0, 10);
    expect(c.costCacheRead()).toBeCloseTo(0.5, 10);
    expect(c.costCacheWrite()).toBeCloseTo(6.25, 10);
  });

  it('totals the four components', () => {
    const c = build({ input: 1_000_000, output: 1_000_000, cacheRead: 1_000_000, cacheWrite: 1_000_000 });
    expect(c.totalCost()).toBeCloseTo(5.0 + 25.0 + 0.5 + 6.25, 10);
  });

  it('prices output at five times input, per the rate table', () => {
    const c = build({ input: 1_000_000, output: 1_000_000 });
    expect(c.costOutput()).toBeCloseTo(c.costInput() * 5, 10);
  });

  it('prices a cache read at a tenth of a fresh input token', () => {
    const c = build({ input: 1_000_000, cacheRead: 1_000_000 });
    expect(c.costCacheRead()).toBeCloseTo(c.costInput() / 10, 10);
  });

  // ── the savings story ──────────────────────────────────────────────────────

  it('values cache reads at what they would have cost as fresh input', () => {
    const c = build({ cacheRead: 1_000_000 });
    expect(c.cacheReadFullPrice()).toBeCloseTo(1_000_000 * RATE.input, 10);
  });

  it('reports savings as the gap between full price and cache price', () => {
    const c = build({ cacheRead: 1_000_000 });
    expect(c.cacheSavings()).toBeCloseTo(1_000_000 * (RATE.input - RATE.cacheRead), 10);
  });

  it('claims no savings when nothing was read from cache', () => {
    expect(build({ input: 500_000 }).cacheSavings()).toBe(0);
  });

  it('never reports a negative saving', () => {
    // A cache read is always cheaper than fresh input, so this can only go one way.
    for (const cacheRead of [0, 1, 1_000, 10_000_000]) {
      expect(build({ cacheRead }).cacheSavings()).toBeGreaterThanOrEqual(0);
    }
  });

  // ── hit rate ───────────────────────────────────────────────────────────────

  it('reports a zero hit rate for an empty session rather than NaN', () => {
    const c = build();
    expect(c.cacheHitRate()).toBe(0);
    expect(Number.isNaN(c.cacheHitRate())).toBe(false);
  });

  it('computes the hit rate over cache reads, writes and fresh input', () => {
    const c = build({ input: 100, cacheRead: 300, cacheWrite: 100 });
    expect(c.cacheHitRate()).toBeCloseTo(300 / 500, 10);
  });

  it('reports a perfect hit rate when everything came from cache', () => {
    expect(build({ cacheRead: 1_000 }).cacheHitRate()).toBe(1);
  });

  it('reports a zero hit rate when nothing came from cache', () => {
    expect(build({ input: 1_000, cacheWrite: 500 }).cacheHitRate()).toBe(0);
  });

  // ── formatters ─────────────────────────────────────────────────────────────

  it('formats money to two decimal places with a dollar sign', () => {
    const c = build();
    expect(c.fmt(0)).toBe('$0.00');
    expect(c.fmt(1.5)).toBe('$1.50');
    expect(c.fmt(1.005)).toBe('$1.00');
    expect(c.fmt(1234.5678)).toBe('$1234.57');
  });

  it('formats percentages as whole numbers', () => {
    const c = build();
    expect(c.fmtPct(0)).toBe('0%');
    expect(c.fmtPct(0.5)).toBe('50%');
    expect(c.fmtPct(0.666)).toBe('67%');
    expect(c.fmtPct(1)).toBe('100%');
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders the total cost into the DOM', () => {
    const c = build({ output: 1_000_000 });
    expect((fixture.nativeElement as HTMLElement).innerHTML).toContain(c.fmt(c.totalCost()));
  });

  it('recomputes when the totals input changes', () => {
    const c = build({ output: 1_000_000 });
    const before = c.totalCost();
    fixture.componentRef.setInput('totals', {
      input: 0,
      output: 2_000_000,
      cacheRead: 0,
      cacheWrite: 0,
    });
    fixture.detectChanges();
    expect(c.totalCost()).toBeCloseTo(before * 2, 10);
  });
});
