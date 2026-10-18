import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Cost } from './cost';
import type { CostTotals } from '../index';
import { price } from '../../prices';
import type { ModelTokens } from '../../prices';
import { PRICE_TABLE } from '../../tauri-bridge.fake';

/**
 * The cost panel is the one place a wrong number costs the user money in
 * confidence. Every figure is priced per model from the backend's table; the
 * panel shows what it is given and says when that leaves something out.
 */
describe('Cost', () => {
  let fixture: ComponentFixture<Cost>;

  /**
   * The panel for one model's tokens at standard speed, routed globally, every write's duration
   * recorded, so every figure is exact: Opus 4.8 unless a test names another.
   */
  function build(over: Partial<CostTotals> = {}, model = 'claude-opus-4-8'): Cost {
    const totals: CostTotals = {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      ...over,
    };
    return show(totals, [row(model, totals)]);
  }

  function row(model: string, t: Partial<ModelTokens>): ModelTokens {
    return {
      model, speed: 'standard', inferenceGeo: 'global', input: 0, output: 0, cacheRead: 0,
      cacheWrite: 0, cacheWrite1h: 0, cacheWriteUnknown: 0, ...t,
    };
  }

  function tags(): string[] {
    return [...(fixture.nativeElement as HTMLElement).querySelectorAll('.partial-tag')]
      .map((t) => t.textContent?.trim() ?? '');
  }

  /** The panel for `totals`, priced from `rows`. */
  function show(totals: CostTotals, rows: ModelTokens[]): Cost {
    fixture = TestBed.createComponent(Cost);
    fixture.componentRef.setInput('totals', totals);
    fixture.componentRef.setInput('priced', price(rows, PRICE_TABLE));
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function html(): string {
    return (fixture.nativeElement as HTMLElement).innerHTML;
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

  it('prices a Sonnet session at Sonnet rates and a Haiku one at Haiku rates', () => {
    expect(build({ output: 1_000_000 }, 'claude-sonnet-4-6').totalCost()).toBeCloseTo(15, 10);
    expect(build({ output: 1_000_000 }, 'claude-haiku-4-5-20251001').totalCost()).toBeCloseTo(5, 10);
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

  it('reports savings as the gap between full price and cache price', () => {
    // Opus 4.8: $5/M fresh, $0.50/M from cache.
    expect(build({ cacheRead: 1_000_000 }).cacheSavings()).toBeCloseTo(4.5, 10);
    // Sonnet 5.5: $2/M fresh, $0.10/M from cache.
    expect(build({ cacheRead: 1_000_000 }, 'claude-sonnet-5-5').cacheSavings()).toBeCloseTo(1.9, 10);
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

  it('renders the total cost into the DOM, untagged when it is exact', () => {
    const c = build({ output: 1_000_000 });
    expect(html()).toContain(c.fmt(c.totalCost()));
    expect(tags()).toEqual([]);
  });

  it('recomputes when the priced input changes', () => {
    const c = build({ output: 1_000_000 });
    const before = c.totalCost();
    fixture.componentRef.setInput(
      'priced',
      price([row('claude-opus-4-8', { output: 2_000_000 })], PRICE_TABLE),
    );
    fixture.detectChanges();
    expect(c.totalCost()).toBeCloseTo(before * 2, 10);
  });

  // ── models with no price ───────────────────────────────────────────────────

  it('shows a dash, not $0.00, when no token in the session has a price', () => {
    const c = build({ output: 1_000_000 }, '<synthetic>');
    expect(c.totalCost()).toBe(0);
    expect(c.money('total')).toBe('—');
    const values = [...(fixture.nativeElement as HTMLElement).querySelectorAll('.tile__value')];
    expect(values.map((v) => v.textContent?.trim())).toEqual(['—', '—']);
    expect(html()).not.toContain('$');
    expect(tags()[0]).toBe('no price');
  });

  it('marks a total that leaves a model out as a floor, and names the model', () => {
    const totals = { input: 0, output: 1_000_010, cacheRead: 0, cacheWrite: 0 };
    const c = show(totals, [
      row('claude-opus-4-8', { output: 1_000_000 }),
      row('<synthetic>', { output: 10 }),
    ]);
    expect(c.totalCost()).toBeCloseTo(25, 10);
    expect(c.tag('total')).toEqual({ label: 'at least', why: 'No price for <synthetic>.' });
    // The output figure is the one it is short in; the saving has no cache reads to miss.
    expect(c.tag('output')?.label).toBe('at least');
    expect(c.tag('savings')).toBeNull();
    expect(tags()).toEqual(['at least', 'at least']);
  });

  it('tags the caching saving as well when a model with cache reads has no price', () => {
    const totals = { input: 0, output: 0, cacheRead: 1_000_010, cacheWrite: 0 };
    const c = show(totals, [
      row('claude-opus-4-8', { cacheRead: 1_000_000 }),
      row('<synthetic>', { cacheRead: 10 }),
    ]);
    expect(c.money('savings')).toBe('$4.50');
    expect(c.tag('savings')?.label).toBe('at least');
  });

  it('tags only the cache write and the total when a write recorded no duration', () => {
    const totals = { input: 0, output: 100, cacheRead: 0, cacheWrite: 1_000 };
    const c = show(totals, [row('claude-sonnet-4-6', { output: 100, cacheWrite: 1_000, cacheWriteUnknown: 1_000 })]);
    expect(c.tag('total')?.label).toBe('at least');
    expect(c.tag('cacheWrite')?.why).toContain('5-minute rate');
    expect(c.tag('output')).toBeNull();
    expect(c.tag('savings')).toBeNull();
  });

  // ── Popover fit: figures must not outgrow their column ────────────────────
  //
  // Measured with a real browser at 320x400 (the popover's actual size): each
  // tile gets a 111px content column, and "$1134.87" needed the full width while
  // "SAVED BY CACHING ⓘ" needed 107px inside a 106.6px box and wrapped to two
  // lines, pushing the second tile 7.9px below the window so its figure was cut.
  //
  // jsdom has no layout engine, so these assert the length logic that drives the
  // size classes. The geometry itself is verified by browser measurement, not
  // here — see the 1.2.0 notes.

  it('leaves short figures at full size', () => {
    const c = build({});
    for (const s of ['$0.00', '$1.50', '$158.65']) {
      expect(c.isLong(s)).toBe(false);
      expect(c.isXLong(s)).toBe(false);
    }
  });

  it('shrinks a figure once it reaches the width of its column', () => {
    // $1134.87 is 8 characters, which is exactly where 1.15rem stops fitting.
    const c = build({});
    expect(c.isLong('$1134.87')).toBe(true);
    expect(c.isXLong('$1134.87')).toBe(false);
  });

  it('shrinks further for figures that would overflow even at the reduced size', () => {
    const c = build({});
    expect(c.isXLong('$123456.78')).toBe(true);
    expect(c.isLong('$123456.78')).toBe(false);
  });

  it('never reports a figure as both long and extra-long', () => {
    // The two classes set conflicting font sizes, so they must be exclusive.
    const c = build({});
    for (let n = 0; n <= 14; n++) {
      const s = '$' + '1'.repeat(n);
      expect(c.isLong(s) && c.isXLong(s)).toBe(false);
    }
  });

  it('applies the size class to the rendered figure', () => {
    // A large cache saving is the case from the report: cacheRead priced at the
    // input rate minus the cache rate yields a four-figure sum.
    const c = build({ cacheRead: 300_000_000 });
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    const values = [...el.querySelectorAll('.tile__value')];
    const saving = values[1];
    expect(c.fmt(c.cacheSavings()).length).toBeGreaterThanOrEqual(8);
    expect(
      saving.classList.contains('tile__value--long') ||
        saving.classList.contains('tile__value--xlong'),
    ).toBe(true);
    expect(saving.classList.contains('tile__value')).toBe(true);
  });

  it('keeps the info button inside the label so it cannot wrap away from it', () => {
    // The icon on its own line was the visible symptom; the fix is structural —
    // label and button are one nowrap flex row.
    const el = (build({}), fixture.nativeElement as HTMLElement);
    for (const label of el.querySelectorAll('.tile__label')) {
      expect(label.querySelector('.info-btn')).not.toBeNull();
    }
  });
});
