import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Usage } from './usage';
import { RATE } from '../index';
import type { TokenAgg, UsageReport } from '../index';

/**
 * The usage panel restates the backend rollups as money. Its honesty rules:
 * rolling windows are consumption not quota, the 5h reset is a labelled proxy,
 * and caching savings are reported by Claude Code rather than caused by Lumen.
 */
describe('Usage', () => {
  let fixture: ComponentFixture<Usage>;

  function agg(over: Partial<TokenAgg> = {}): TokenAgg {
    return { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, ...over };
  }

  function build(over: Partial<UsageReport> = {}): Usage {
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
    fixture.detectChanges();
    return fixture.componentInstance;
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

  it('prices the 5h window at the shared rate table', () => {
    const u = build({ rolling5h: agg({ input: 1_000_000, output: 1_000_000 }) });
    expect(u.cost5h()).toBeCloseTo(1_000_000 * RATE.input + 1_000_000 * RATE.output, 10);
  });

  it('combines the opus and other splits for the 7d totals', () => {
    const u = build({
      rolling7dOpus: agg({ turns: 3, input: 1_000_000, totalTokens: 1_000_000 }),
      rolling7dOther: agg({ turns: 7, input: 2_000_000, totalTokens: 2_000_000 }),
    });
    expect(u.turns7d()).toBe(10);
    expect(u.tokens7d()).toBe(3_000_000);
    expect(u.cost7d()).toBeCloseTo(3_000_000 * RATE.input, 10);
  });

  it('prices each calendar rollup independently', () => {
    const u = build({
      today: agg({ output: 1_000_000 }),
      thisWeek: agg({ output: 3_000_000 }),
      allTime: agg({ output: 10_000_000 }),
    });
    expect(u.costToday()).toBeCloseTo(1_000_000 * RATE.output, 10);
    expect(u.costWeek()).toBeCloseTo(3_000_000 * RATE.output, 10);
    expect(u.costAll()).toBeCloseTo(10_000_000 * RATE.output, 10);
  });

  // ── caching savings ────────────────────────────────────────────────────────

  it('values lifetime caching savings as the input/cache-read spread', () => {
    const u = build({ allTime: agg({ cacheRead: 1_000_000 }) });
    expect(u.savedByCaching()).toBeCloseTo(1_000_000 * (RATE.input - RATE.cacheRead), 10);
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
    expect(u.fmt(0)).toBe('$0.00');
    expect(u.fmt(12.345)).toBe('$12.35');
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
