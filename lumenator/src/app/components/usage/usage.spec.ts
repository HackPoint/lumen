import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Usage } from './usage';
import type { ModelAgg, TokenAgg, UsageReport } from '../index';
import type { PriceTable } from '../../prices';
import { PRICE_TABLE } from '../../tauri-bridge.fake';

/**
 * The usage panel restates the backend rollups as money, each model's share at that
 * model's prices. Its honesty rules: rolling windows are consumption not quota, the 5h
 * reset is a labelled proxy, caching savings are reported by Claude Code rather than
 * caused by Lumen, and a figure that leaves out a model with no price says so.
 */
describe('Usage', () => {
  let fixture: ComponentFixture<Usage>;

  /** An aggregate of one model's turns, Opus 4.8 unless a test names another. */
  function agg(over: Partial<TokenAgg> = {}, model = 'claude-opus-4-8'): TokenAgg {
    const a = { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, ...over };
    return { byModel: [row(model, a)], ...a };
  }

  /**
   * One model's share at standard speed, routed globally, every write's duration recorded: an
   * exact figure.
   */
  function row(model: string | null, over: Partial<ModelAgg> = {}): ModelAgg {
    return {
      model, speed: 'standard', inferenceGeo: 'global', turns: 0, input: 0, output: 0,
      cacheRead: 0, cacheWrite: 0, cacheWrite1h: 0, cacheWriteUnknown: 0, ...over,
    };
  }

  function tagTexts(): string[] {
    return [...(fixture.nativeElement as HTMLElement).querySelectorAll('.partial-tag')]
      .map((t) => t.textContent?.trim() ?? '');
  }

  function build(over: Partial<UsageReport> = {}, prices: PriceTable | null = PRICE_TABLE): Usage {
    const report: UsageReport = {
      rolling5h: agg(),
      windowStart: null,
      resetApprox: null,
      rolling7dOpus: agg(),
      rolling7dOther: agg(),
      today: agg(),
      thisWeek: agg(),
      allTime: agg(),
      ...over,
    } as UsageReport;
    fixture = TestBed.createComponent(Usage);
    fixture.componentRef.setInput('report', report);
    fixture.componentRef.setInput('prices', prices);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  // ── window costs ───────────────────────────────────────────────────────────

  it('is all zeros for an empty report', () => {
    const u = build();
    expect(u.cost5h()).toBe(0);
    expect(u.cost7d()).toBe(0);
    expect(u.costToday()).toBe(0);
    expect(u.costWeek()).toBe(0);
    expect(u.costAll()).toBe(0);
    expect(u.savedByCaching()).toBe(0);
  });

  it("prices the 5h window at its model's rates", () => {
    const u = build({ rolling5h: agg({ input: 1_000_000, output: 1_000_000 }) });
    expect(u.cost5h()).toBeCloseTo(5 + 25, 10);
  });

  it("prices each model's share of a window at that model's rates", () => {
    const today: TokenAgg = {
      ...agg({ output: 3_000_000 }),
      byModel: [
        row('claude-opus-4-8', { output: 1_000_000 }),
        row('claude-sonnet-4-6', { output: 1_000_000 }),
        row('claude-haiku-4-5-20251001', { output: 1_000_000 }),
      ],
    };
    expect(build({ today }).costToday()).toBeCloseTo(25 + 15 + 5, 10);
  });

  it('gives a model with no price no dollars and marks the figure a floor', () => {
    const today: TokenAgg = {
      ...agg({ output: 1_000_010 }),
      byModel: [row('claude-opus-4-8', { output: 1_000_000 }), row('<synthetic>', { output: 10 })],
    };
    const u = build({ today });
    expect(u.costToday()).toBeCloseTo(25, 10);
    expect(u.tag(u.pricedToday())).toEqual({ label: 'at least', why: 'No price for <synthetic>.' });
    expect(tagTexts()).toEqual(['at least']);
  });

  it('marks a window holding writes of unknown duration or turns of unknown speed a floor', () => {
    const week: TokenAgg = {
      ...agg({ cacheWrite: 100 }),
      byModel: [row('claude-sonnet-4-6', { cacheWrite: 100, cacheWriteUnknown: 100 })],
    };
    const all: TokenAgg = {
      ...agg({ output: 10 }),
      byModel: [row('claude-opus-4-8', { speed: null, output: 10 })],
    };
    const u = build({ thisWeek: week, allTime: all });
    expect(u.tag(u.pricedWeek())?.why).toContain('5-minute rate');
    expect(u.tag(u.pricedAll())?.why).toContain('did not record their speed');
    // The saving is a floor for an unknown speed, never for a write's duration.
    expect(u.savedTag(u.pricedAll())?.label).toBeUndefined();
    expect(u.tag(u.pricedToday())).toBeNull();
  });

  it('marks every figure that leaves a model out, and only those', () => {
    const mixed = (): TokenAgg => ({
      ...agg({ output: 1_000_010, cacheRead: 10 }),
      byModel: [row('claude-opus-4-8', { output: 1_000_000 }), row(null, { output: 10, cacheRead: 10 })],
    });
    build({
      rolling5h: mixed(),
      rolling7dOpus: mixed(),
      today: mixed(),
      thisWeek: mixed(),
      allTime: mixed(),
    });
    // 5h, 7d, today, this week, all-time, and the caching saving drawn from all-time.
    expect(tagTexts()).toEqual(Array(6).fill('at least'));

    build();
    expect(tagTexts()).toEqual([]);
  });

  it('shows a dash for a window whose every model is unpriced', () => {
    const u = build({ rolling5h: agg({ output: 1_000 }, 'claude-haiku-5-5') });
    expect(u.money(u.priced5h())).toBe('—');
    expect(u.tag(u.priced5h())?.label).toBe('no price');
  });

  it('shows no dollar figure at all until the price table arrives', () => {
    const u = build({ today: agg({ output: 1_000_000 }) }, null);
    expect(u.costToday()).toBe(0);
    expect(u.pricedToday().hasFigure).toBe(false);
    expect(u.money(u.pricedToday())).toBe('—');
    expect(u.tag(u.pricedToday())?.why).toContain('Prices have not loaded');
    // A window with no tokens costs nothing whatever the prices, and says so.
    expect(u.money(u.priced5h())).toBe('$0.00');
  });

  it('combines the opus and other splits for the 7d totals', () => {
    const u = build({
      rolling7dOpus: agg({ turns: 3, input: 1_000_000, totalTokens: 1_000_000 }),
      rolling7dOther: agg({ turns: 7, input: 2_000_000, totalTokens: 2_000_000 }),
    });
    expect(u.turns7d()).toBe(10);
    expect(u.tokens7d()).toBe(3_000_000);
    expect(u.cost7d()).toBeCloseTo(3 * 5, 10);
  });

  it('prices each calendar rollup independently', () => {
    const u = build({
      today: agg({ output: 1_000_000 }),
      thisWeek: agg({ output: 3_000_000 }),
      allTime: agg({ output: 10_000_000 }),
    });
    expect(u.costToday()).toBeCloseTo(25, 10);
    expect(u.costWeek()).toBeCloseTo(3 * 25, 10);
    expect(u.costAll()).toBeCloseTo(10 * 25, 10);
  });

  // ── caching savings ────────────────────────────────────────────────────────

  it('values lifetime caching savings as the input/cache-read spread', () => {
    const u = build({ allTime: agg({ cacheRead: 1_000_000 }) });
    expect(u.savedByCaching()).toBeCloseTo(5 - 0.5, 10);
    // Sonnet 5.5's cache hit is 0.05 times its input price, not 0.1.
    const sonnet = build({ allTime: agg({ cacheRead: 1_000_000 }, 'claude-sonnet-5-5') });
    expect(sonnet.savedByCaching()).toBeCloseTo(2 - 0.1, 10);
  });

  it('never reports a negative caching saving', () => {
    for (const cacheRead of [0, 1, 1_000_000]) {
      expect(build({ allTime: agg({ cacheRead }) }).savedByCaching()).toBeGreaterThanOrEqual(0);
    }
  });

  // ── reset proxy formatting ─────────────────────────────────────────────────

  it('has no reset time when the backend reports none', () => {
    expect(build().resetApproxLocal()).toBeNull();
  });

  it('formats the SQL datetime as a local time', () => {
    // SQLite hands back "YYYY-MM-DD HH:MM:SS" in UTC with no zone marker.
    const u = build({ resetApprox: '2026-07-28 15:30:00' });
    const formatted = u.resetApproxLocal();
    expect(formatted).not.toBeNull();
    expect(formatted).toBe(
      new Date('2026-07-28T15:30:00Z').toLocaleTimeString([], {
        hour: '2-digit',
        minute: '2-digit',
      }),
    );
  });

  it('returns null rather than "Invalid Date" for an unparseable timestamp', () => {
    expect(build({ resetApprox: 'not a timestamp' }).resetApproxLocal()).toBeNull();
    expect(build({ resetApprox: '' }).resetApproxLocal()).toBeNull();
  });

  // ── formatters ─────────────────────────────────────────────────────────────

  it('formats money and token counts for display', () => {
    const u = build();
    expect(u.money(u.pricedAll())).toBe('$0.00');
    expect(u.savedMoney(u.pricedAll())).toBe('$0.00');
    const big = build({ allTime: agg({ output: 494_000 }) });
    expect(big.money(big.pricedAll())).toBe('$12.35');
    expect(u.fmtTokens(1_234_567)).toBe((1_234_567).toLocaleString());
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders the honest framing rather than implying a quota', () => {
    const html = (() => {
      build({ rolling5h: agg({ turns: 4, input: 1_000_000 }) });
      return (fixture.nativeElement as HTMLElement).textContent ?? '';
    })();
    // Consumption language, and the reset explicitly marked as approximate.
    expect(html.toLowerCase()).not.toContain('remaining');
    expect(html.toLowerCase()).not.toContain('of limit');
  });

  it('recomputes when the report input changes', () => {
    const u = build({ today: agg({ output: 1_000_000 }) });
    const before = u.costToday();
    fixture.componentRef.setInput('report', {
      rolling5h: agg(),
      windowStart: null,
      resetApprox: null,
      rolling7dOpus: agg(),
      rolling7dOther: agg(),
      today: agg({ output: 4_000_000 }),
      thisWeek: agg(),
      allTime: agg(),
    });
    fixture.detectChanges();
    expect(u.costToday()).toBeCloseTo(before * 4, 10);
  });
});
