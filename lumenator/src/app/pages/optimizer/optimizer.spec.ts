import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Optimizer } from './optimizer';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import { RATE } from '../../components/index';
import type { OptimizerReport, UsageReport, TokenAgg } from '../../components/index';

/**
 * The optimizer page is where Lumen makes its central claim: "I saved you this
 * much." Its honesty rules are load-bearing — savings CAUSED by Lumen must never
 * be merged with caching savings REPORTED by Claude Code, and missed reads must
 * never be presented as savings.
 */
describe('Optimizer', () => {
  let fixture: ComponentFixture<Optimizer>;
  let bridge: FakeTauriBridge;

  function agg(over: Partial<TokenAgg> = {}): TokenAgg {
    return { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, ...over };
  }

  function report(over: Partial<OptimizerReport> = {}): OptimizerReport {
    return {
      lifetimeOptimizedTokens: 0,
      lifetimeFullTokens: 0,
      todaySavedTokens: 0,
      thisWeekSavedTokens: 0,
      byChannel: [],
      byTool: [],
      currentChannel: 'unknown',
      missedCalls: 0,
      missedFullTokens: 0,
      ...over,
    } as OptimizerReport;
  }

  function usage(over: Partial<UsageReport> = {}): UsageReport {
    return {
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
  }

  /** Build the page with the given backend reports already resolved. */
  async function build(
    optimizerReport = report(),
    usageReport = usage(),
  ): Promise<Optimizer> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_optimizer_stats', optimizerReport);
    bridge.responses.set('get_usage', usageReport);
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: TauriBridge, useValue: bridge },
        SessionService,
      ],
    });
    fixture = TestBed.createComponent(Optimizer);
    fixture.detectChanges();
    // Let the constructor's two refresh calls resolve.
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── empty state ────────────────────────────────────────────────────────────

  it('reports no data before any optimized read', async () => {
    const o = await build();
    expect(o.hasData()).toBe(false);
    expect(o.effectPct()).toBe(0);
  });

  it('never shows NaN or a negative effectiveness', async () => {
    const o = await build(report({ lifetimeOptimizedTokens: 500, lifetimeFullTokens: 0 }));
    expect(o.effectPct()).toBe(0);
    expect(text()).not.toContain('NaN');
  });

  it('rounds effectiveness to one decimal', async () => {
    const o = await build(
      report({ lifetimeOptimizedTokens: 6_666, lifetimeFullTokens: 10_000 }),
    );
    expect(o.effectPct()).toBe(66.7);
  });

  it('flags that it has data once something was optimized', async () => {
    const o = await build(report({ lifetimeOptimizedTokens: 1 }));
    expect(o.hasData()).toBe(true);
  });

  // ── mode banner ────────────────────────────────────────────────────────────

  it('describes CLI as full mode with interception', async () => {
    const o = await build(report({ currentChannel: 'cli' }));
    expect(o.modeLabel()).toBe('Full mode');
    expect(o.fireflyState()).toBe('full');
    expect(o.modeDesc()).toContain('intercepted');
  });

  it('describes VS Code as soft mode and says what is invisible there', async () => {
    const o = await build(report({ currentChannel: 'vscode' }));
    expect(o.modeLabel()).toBe('Soft mode');
    expect(o.fireflyState()).toBe('soft');
    expect(o.modeDesc()).toContain('not enforced');
    expect(o.modeDesc()).toContain('CLI');
  });

  it('says "no activity yet" rather than implying a mode', async () => {
    const o = await build(report({ currentChannel: 'unknown' }));
    expect(o.modeLabel()).toBe('No activity yet');
    expect(o.fireflyState()).toBe('idle');
  });

  // ── hero colour thresholds ─────────────────────────────────────────────────

  it.each([
    [9_000, 10_000, 'var(--lumen-ok)'], // 90%
    [8_000, 10_000, 'var(--lumen-ok)'], // exactly 80%
    [7_900, 10_000, 'var(--lumen-warn)'], // 79%
    [5_000, 10_000, 'var(--lumen-warn)'], // exactly 50%
    [4_900, 10_000, 'var(--lumen-text-dim)'], // 49%
    [0, 0, 'var(--lumen-text-dim)'],
  ])('colours %i/%i as %s', async (saved, full, expected) => {
    const o = await build(
      report({ lifetimeOptimizedTokens: saved, lifetimeFullTokens: full }),
    );
    expect(o.heroColor()).toBe(expected);
  });

  it('colours by the rounded figure it displays, not the raw one', async () => {
    // 79.99% rounds to "80.0%" for display, so it must also take the 80% colour.
    // A page showing 80.0% in the warn colour would contradict itself.
    const o = await build(
      report({ lifetimeOptimizedTokens: 7_999, lifetimeFullTokens: 10_000 }),
    );
    expect(o.effectPct()).toBe(80);
    expect(o.heroColor()).toBe('var(--lumen-ok)');
  });

  // ── the two savings numbers must stay separate ─────────────────────────────

  it('keeps caching savings (reported) distinct from optimizer savings (caused)', async () => {
    const o = await build(
      report({ lifetimeOptimizedTokens: 1_000_000 }),
      usage({ allTime: agg({ cacheRead: 5_000_000 }) }),
    );
    // Caused by Lumen: priced at the input rate.
    expect(o.s.lifetimeOptimizedUsd()).toBeCloseTo(1_000_000 * RATE.input, 10);
    // Reported by Claude Code: priced at the input/cache-read spread.
    expect(o.cacheSavedUsd()).toBeCloseTo(5_000_000 * (RATE.input - RATE.cacheRead), 10);
    // And they are different numbers, never summed into one headline.
    expect(o.cacheSavedUsd()).not.toBeCloseTo(o.s.lifetimeOptimizedUsd(), 10);
  });

  it('reports zero caching savings when the usage report has not loaded', async () => {
    const o = await build(report({ lifetimeOptimizedTokens: 100 }));
    expect(o.cacheSavedTokens()).toBe(0);
    expect(o.cacheSavedUsd()).toBe(0);
  });

  // ── missed reads ───────────────────────────────────────────────────────────

  it('shows missed reads only in CLI mode, where the hook actually fires', async () => {
    const cli = await build(report({ currentChannel: 'cli', missedCalls: 3 }));
    expect(cli.showMissed()).toBe(true);

    TestBed.resetTestingModule();
    const vscode = await build(report({ currentChannel: 'vscode', missedCalls: 3 }));
    expect(vscode.showMissed()).toBe(false);
  });

  it('hides the missed-reads block when there are none', async () => {
    const o = await build(report({ currentChannel: 'cli', missedCalls: 0 }));
    expect(o.showMissed()).toBe(false);
  });

  // ── breakdown bars ─────────────────────────────────────────────────────────

  it('scales bars against the largest row', async () => {
    const o = await build(
      report({
        byTool: [
          { tool: 'smart_read', calls: 1, savedTokens: 1_000, fullTokens: 2_000 },
          { tool: 'recall_file', calls: 1, savedTokens: 500, fullTokens: 1_000 },
        ],
      }),
    );
    expect(o.maxToolSaved()).toBe(1_000);
    expect(o.barPct(1_000, o.maxToolSaved())).toBe(100);
    expect(o.barPct(500, o.maxToolSaved())).toBe(50);
  });

  it('never divides by zero when every row is empty', async () => {
    const o = await build(
      report({ byTool: [{ tool: 'smart_read', calls: 1, savedTokens: 0, fullTokens: 0 }] }),
    );
    expect(o.maxToolSaved()).toBe(1); // floor of 1 keeps the division safe
    expect(o.barPct(0, o.maxToolSaved())).toBe(0);
  });

  it('returns 0% for a zero maximum rather than NaN', async () => {
    const o = await build();
    expect(o.barPct(5, 0)).toBe(0);
  });

  it('has a max of 1 when there are no rows at all', async () => {
    const o = await build();
    expect(o.maxToolSaved()).toBe(1);
    expect(o.maxChanSaved()).toBe(1);
  });

  // ── what the breakdowns render ─────────────────────────────────────────────

  function rows(section: string): string[] {
    return Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll(`section[aria-label="${section}"] .breakdown__row`),
    ).map((r) => (r.textContent ?? '').replace(/\s+/g, ' ').trim());
  }

  it('renders a row per channel, labelled with its mode', async () => {
    await build(report({
      lifetimeOptimizedTokens: 1_500,
      lifetimeFullTokens: 3_000,
      byChannel: [
        { channel: 'cli', calls: 2, savedTokens: 1_000, fullTokens: 2_000 },
        { channel: 'vscode', calls: 1, savedTokens: 500, fullTokens: 1_000 },
      ],
    }));
    expect(rows('Savings by channel')).toEqual([
      `CLI (Full mode) ${(1_000).toLocaleString()} tok · 2 calls`,
      `VS Code (Soft mode) ${(500).toLocaleString()} tok · 1 call`,
    ]);
  });

  it('renders no channel section until a channel has a row', async () => {
    await build();
    expect(fixture.nativeElement.querySelector('section[aria-label="Savings by channel"]')).toBeNull();
  });

  it('says "1 call", not "1 calls", for a single tool call', async () => {
    await build(report({
      lifetimeOptimizedTokens: 10,
      lifetimeFullTokens: 20,
      byTool: [{ tool: 'mcp__lumen__smart_read', calls: 1, savedTokens: 10, fullTokens: 20 }],
    }));
    expect(text()).toContain('tok · 1 call');
    expect(text()).not.toContain('1 calls');
  });

  it('renders the missed reads as context, with their count and tokens', async () => {
    await build(report({ currentChannel: 'cli', missedCalls: 3, missedFullTokens: 12_000 }));
    const note = (fixture.nativeElement as HTMLElement).querySelector('.missed__val');
    expect(note?.textContent?.replace(/\s+/g, ' ').trim()).toBe(`3 calls · ${(12_000).toLocaleString()} tokens`);
    expect(text()).toContain('never counted as savings');
  });

  it('says "1 call" for a single missed read', async () => {
    await build(report({ currentChannel: 'cli', missedCalls: 1, missedFullTokens: 40 }));
    expect((fixture.nativeElement as HTMLElement).querySelector('.missed__val')?.textContent)
      .toMatch(/^\s*1 call\s/);
  });

  it('badges the Hotspots tab with the recorded fault count', async () => {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_optimizer_stats', report());
    bridge.responses.set('get_usage', usage());
    bridge.responses.set('get_fault_count', 2);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Optimizer);
    fixture.detectChanges();
    for (let i = 0; i < 4; i++) await Promise.resolve();
    fixture.detectChanges();
    const badge = (fixture.nativeElement as HTMLElement).querySelector('.tab-nav__badge');
    expect(badge?.textContent?.trim()).toBe('2');
    expect(badge?.getAttribute('aria-label')).toBe('2 faults recorded');
  });

  // ── labels ─────────────────────────────────────────────────────────────────

  it('strips the mcp prefix from tool names', async () => {
    const o = await build();
    const row = (tool: string) => ({ tool, calls: 0, savedTokens: 0, fullTokens: 0 });
    expect(o.toolLabel(row('mcp__lumen__smart_read'))).toBe('smart_read');
    expect(o.toolLabel(row('mcp__lumen__recall_file'))).toBe('recall_file');
    expect(o.toolLabel(row('mcp__lumen__compress_logs'))).toBe('compress_logs');
  });

  it('passes an unrecognised tool name through unchanged', async () => {
    const o = await build();
    expect(o.toolLabel({ tool: 'something_new', calls: 0, savedTokens: 0, fullTokens: 0 })).toBe(
      'something_new',
    );
  });

  it('labels channels with their mode', async () => {
    const o = await build();
    const row = (channel: string) => ({ channel, calls: 0, savedTokens: 0, fullTokens: 0 });
    expect(o.chanLabel(row('cli'))).toBe('CLI (Full mode)');
    expect(o.chanLabel(row('vscode'))).toBe('VS Code (Soft mode)');
    expect(o.chanLabel(row('unknown'))).toBe('unknown');
  });

  // ── currency formatting ────────────────────────────────────────────────────

  it('scales USD precision so tiny savings are not rounded to $0.00', async () => {
    const o = await build();
    expect(o.fmtUsd(12.3456)).toBe('$12.35'); // >= $10: two decimals
    expect(o.fmtUsd(1.23456)).toBe('$1.2346'); // >= 1c: four decimals
    expect(o.fmtUsd(0.0000123)).toBe('$0.000012'); // sub-cent: six decimals
    expect(o.fmtUsd(0)).toBe('$0.000000');
  });

  it('formats token counts with separators', async () => {
    const o = await build();
    expect(o.fmtTokens(1_234_567)).toBe((1_234_567).toLocaleString());
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders without a Tauri runtime', async () => {
    await build();
    expect(text().length).toBeGreaterThan(0);
  });

  it('never renders the word "remaining", which would imply a quota', async () => {
    await build(report({ lifetimeOptimizedTokens: 1_000, lifetimeFullTokens: 2_000 }));
    expect(text().toLowerCase()).not.toContain('remaining');
  });

  // ── Token provenance ───────────────────────────────────────────────────────
  //
  // "Never estimated" and "verifiable" are said only over a ledger that supports them.
  // These read the rendered page, not the signals behind it: the sentence on screen is
  // the claim, and it is what has to change when the data cannot back it.

  describe('token provenance', () => {
    const data = { lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000 };

    function rendered(selector: string): string {
      const el = (fixture.nativeElement as HTMLElement).querySelector(selector);
      return (el?.textContent ?? '').replace(/\s+/g, ' ').trim();
    }

    it('says exact, and verifiable, when every count was measured', async () => {
      await build(report({ ...data, unverifiedProvenanceRows: 0, provenanceTotalRows: 7_192 }));
      expect(rendered('.hero__secondary'))
        .toContain('Counted to the token by a local tokenizer, never estimated.');
      expect(rendered('.savings-card--caused .savings-card__note'))
        .toBe('caused · measured · verifiable · grows with use');
      expect(fixture.nativeElement.querySelector('.hero__warn')).toBeNull();
      expect(text()).not.toContain('unverified');
    });

    it('says N of M unverified, and not verifiable, when some counts were not measured', async () => {
      await build(report({ ...data, unverifiedProvenanceRows: 2_584, provenanceTotalRows: 7_192 }));
      expect(rendered('.hero__secondary')).not.toContain('never estimated');
      expect(rendered('.hero__secondary'))
        .toContain('Counts are exact for Lumen tool calls; some older events have unverified provenance.');
      expect(rendered('.hero__warn')).toContain(
        `${(2_584).toLocaleString()} of ${(7_192).toLocaleString()} metered events have `
        + 'unverified token provenance',
      );
      expect(rendered('.savings-card--caused .savings-card__note'))
        .toBe('caused · partly unverified · grows with use');
    });

    it('qualifies the claim over a single unverified count', async () => {
      // The boundary: one row is enough to make "never estimated" false.
      await build(report({ ...data, unverifiedProvenanceRows: 1, provenanceTotalRows: 1 }));
      expect(rendered('.hero__warn')).toContain('1 of 1 metered events');
      expect(rendered('.savings-card--caused .savings-card__note')).toContain('partly unverified');
      expect(text()).not.toContain('never estimated');
    });
  });


  // ── The dollar headline (1.4.0) ────────────────────────────────────────────
  //
  // Published per a pre-committed rule: whatever the number is, it renders. A metric
  // that only displays well when it flatters is not a measurement, and the token ratio
  // it replaced could not be wrong in the direction that mattered — a smaller reply
  // that forces another round is a loss however good the ratio looks.

  describe('net dollar value', () => {
    it('leads with the dollar figure, not the token ratio', async () => {
      await build(report({
        lifetimeOptimizedTokens: 1_000_000,
        lifetimeFullTokens: 1_200_000,
        netValueUsd: 275.93,
        grossValueUsd: 386.72,
        roundCostUsd: 110.79,
        valueRounds: 194,
        pairMultiplier: 1.604,
      }));
      const hero = fixture.nativeElement.querySelector('.hero__num') as HTMLElement;
      expect(hero.textContent).toContain('+$276');
      expect(hero.textContent).not.toContain('%');
      // The ratio is still on the page, demoted.
      expect(fixture.nativeElement.querySelector('.hero__secondary')?.textContent)
        .toContain('fewer tokens');
    });

    it('renders a negative result as negative, with no softening', async () => {
      await build(report({
        lifetimeOptimizedTokens: 300,
        lifetimeFullTokens: 400,
        netValueUsd: -42.5,
        grossValueUsd: 10,
        roundCostUsd: 52.5,
        valueRounds: 194,
      }));
      const hero = fixture.nativeElement.querySelector('.hero__num') as HTMLElement;
      expect(hero.textContent).toContain('−$42.50');
      const t = text().toLowerCase();
      for (const softener of ['but ', 'still', 'however', 'nonetheless', 'despite']) {
        expect(t).not.toContain(softener);
      }
    });

    it('says break-even rather than rounding it into a win', async () => {
      // lifetime tokens must be non-zero or hasData() is false and the empty state
      // renders instead of the hero — the test would then assert nothing.
      await build(report({
        lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000,
        netValueUsd: 0.4, grossValueUsd: 50, roundCostUsd: 49.6, valueRounds: 194,
      }));
      expect(text()).toContain('roughly break-even');
      expect(fixture.nativeElement.querySelector('.hero__label')?.textContent)
        .not.toContain('net value');
    });

    it('claims no figure at all when a round cannot be priced', async () => {
      await build(report({
        lifetimeOptimizedTokens: 1_000,
        lifetimeFullTokens: 2_000,
        netValueUsd: 0,
        grossValueUsd: 0,
        roundCostUsd: 0,
      }));
      expect(text()).toContain('Not enough recorded turns yet to price a round');
      // Not "+$0.00, roughly break-even" beside it: that is a figure, and a verdict.
      const hero = fixture.nativeElement.querySelector('.hero__num') as HTMLElement;
      expect(hero.textContent?.trim()).toBe('—');
      expect(fixture.nativeElement.querySelector('.hero__ring')?.textContent).not.toContain('$');
      expect(text()).not.toContain('break-even');
    });

    it('surfaces R, because the result is most sensitive to it', async () => {
      await build(report({
        lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000,
        netValueUsd: 100, grossValueUsd: 200, roundCostUsd: 100, valueRounds: 194,
      }));
      expect(text()).toContain('194 rounds');
      expect(text()).toContain('sensitive');
    });

    it('still renders against a backend that predates the field', async () => {
      // 1.3.x sends no netValueUsd at all; the page must not show NaN.
      const r = report({ lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000 });
      const bare = r as unknown as Record<string, unknown>;
      delete bare['netValueUsd'];
      delete bare['roundCostUsd'];
      await build(r);
      expect(text()).not.toContain('NaN');
      expect(text()).toContain('Not enough recorded turns yet to price a round');
    });
  });
});
