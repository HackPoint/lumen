import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Optimizer } from './optimizer';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import { RATE } from '../../components/index';
import type {
  OptimizerReport,
  RepeatReads,
  RepeatReadsSetting,
  TokenAgg,
  UsageReport,
} from '../../components/index';

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

  const SETTINGS_FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: SETTINGS_FILE, ...over };
  }

  function repeats(over: Partial<RepeatReads> = {}): RepeatReads {
    return {
      unchangedServed: 0,
      deltasServed: 0,
      tokensNotResent: 0,
      measuredAnswers: 0,
      freshRequests: 0,
      unidentifiedReads: 0,
      ...over,
    };
  }

  /** Build the page with the given backend reports already resolved. */
  async function build(
    optimizerReport = report(),
    usageReport = usage(),
    seed?: (b: FakeTauriBridge) => void,
  ): Promise<Optimizer> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_optimizer_stats', optimizerReport);
    bridge.responses.set('get_usage', usageReport);
    // No settings file: repeat reads are on by default.
    bridge.responses.set('get_repeat_reads_setting', setting());
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: TauriBridge, useValue: bridge },
        SessionService,
      ],
    });
    fixture = TestBed.createComponent(Optimizer);
    fixture.detectChanges();
    await settle();
    return fixture.componentInstance;
  }

  /** Let the constructor's refreshes and the setting's read resolve, then render. */
  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  /** What one element renders, whitespace collapsed. */
  function rendered(selector: string): string {
    const el = (fixture.nativeElement as HTMLElement).querySelector(selector);
    return (el?.textContent ?? '').replace(/\s+/g, ' ').trim();
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

  // ── Repeat reads ───────────────────────────────────────────────────────────
  //
  // The card counts answers that are in none of the totals above it. Every test reads what the
  // card says on screen: the wording is the claim, and each state has to say what is true of it
  // and nothing more.

  describe('repeat reads card', () => {
    const CARD = 'section[aria-label="Repeat reads"]';
    const FILE_OFF = setting({ on: false, source: 'file' });
    const CORRUPT = setting({
      on: false,
      source: 'file',
      problem: 'not JSON: expected value at line 1 column 1',
    });
    const n = (v: number): string => v.toLocaleString();
    // 1,540 answers served short and 60 requests for the full result: 1,600 repeat reads.
    const SERVED = repeats({
      unchangedServed: 1_200,
      deltasServed: 340,
      tokensNotResent: 4_125_000,
      measuredAnswers: 1_540,
      freshRequests: 60,
    });
    const BASELINE =
      'Counted against reading the whole file again, not against the outline Lumen sends with repeat reads off.';
    const ENV_CAVEAT =
      'A LUMEN_REPEAT_READS value in a Claude Code MCP config takes priority over the settings file, and this window cannot see it.';
    const OFF_FILE =
      'Repeat reads are off now: the settings file says off, so every read returns its full result.';
    const NONE_YET = 'No repeat read has been answered short yet.';

    /** What the card says, whitespace collapsed. */
    function said(): string {
      expect(fixture.nativeElement.querySelector(CARD)).not.toBeNull();
      return rendered(CARD);
    }

    /** One entry per figure: its name, its value and what it is counted against. */
    function figures(): string[] {
      return Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll(`${CARD} .repeat-card__row`),
      ).map((r) => (r.textContent ?? '').replace(/\s+/g, ' ').trim());
    }

    function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
      let resolve!: (v: T) => void;
      const promise = new Promise<T>((r) => {
        resolve = r;
      });
      return { promise, resolve };
    }

    const fileSays = (s: RepeatReadsSetting) => (b: FakeTauriBridge) =>
      b.responses.set('get_repeat_reads_setting', s);
    const cannotRead = (b: FakeTauriBridge) => b.failures.add('get_repeat_reads_setting');

    it('reads the setting when the page opens', async () => {
      await build(report({ repeatReads: repeats() }));
      expect(bridge.countOf('get_repeat_reads_setting')).toBe(1);
    });

    // ── nothing counted ──────────────────────────────────────────────────────

    it('says none yet, and what a repeat read is, with no figure in it', async () => {
      await build(report({ repeatReads: repeats() }));
      expect(said()).toContain(NONE_YET);
      expect(said()).toContain(
        'A repeat read is a file Claude reads again in the same conversation; Lumen can answer it '
        + 'with a short note that the file is unchanged, or with only what an edit changed, and '
        + 'those answers are counted here.',
      );
      expect(figures()).toEqual([]);
      // Nothing to put a number on: no zero, no ratio, no price.
      expect(said()).not.toMatch(/\d|NaN|[%$]/);
      expect(said()).not.toContain('off now');
    });

    it('says repeat reads are off, as the settings file says, when it turns them off and none were counted', async () => {
      await build(report({ repeatReads: repeats() }), usage(), fileSays(FILE_OFF));
      expect(said()).toContain(OFF_FILE);
      expect(said()).toContain(ENV_CAVEAT);
      expect(said()).toContain(NONE_YET);
      expect(figures()).toEqual([]);
      expect(said()).not.toMatch(/\d|NaN|[%$]/);
    });

    // ── answers served ───────────────────────────────────────────────────────

    it('shows the four figures, each with what it is counted against', async () => {
      await build(report({ repeatReads: SERVED }));
      expect(said()).toContain('Answered so far, for files Claude had already read in the same conversation.');
      expect(figures()).toEqual([
        `Unchanged answers served ${n(1_200)} A short note instead of the whole result.`,
        `Edits served as a delta ${n(340)} Only what the edit changed.`,
        `Tokens not re-sent ${n(4_125_000)} ${BASELINE}`,
        `Full result asked for again (fresh=true) ${n(60)} of ${n(1_600)} `
          + `${n(1_600)} = ${n(1_200)} unchanged + ${n(340)} deltas + ${n(60)} asked again.`,
      ]);
      expect(said()).not.toContain(NONE_YET);
      expect(said()).not.toContain('off now');
      expect(said()).not.toContain(ENV_CAVEAT);
    });

    it('says the answers are counted apart from the totals above, and which totals', async () => {
      await build(report({ repeatReads: SERVED }));
      expect(said()).toContain(
        'Counted apart from the totals above: these answers are in none of the token totals, '
        + 'the by-tool and by-channel breakdowns, or the net value.',
      );
    });

    it('leaves every figure above it as it was', async () => {
      const totals = {
        lifetimeOptimizedTokens: 900,
        lifetimeFullTokens: 1_000,
        netValueUsd: 100,
        grossValueUsd: 200,
        roundCostUsd: 100,
        valueRounds: 194,
      };
      const above = (): string[] =>
        ['.hero__num', '.savings-card--caused .savings-card__num', '.period-row'].map(rendered);
      await build(report(totals));
      const before = above();
      TestBed.resetTestingModule();
      await build(report({ ...totals, repeatReads: SERVED }));
      expect(above()).toEqual(before);
      expect(before.slice(0, 2)).toEqual(['+$100', '900']);
    });

    it('never words a figure as a saving, and shows no dollars or percentage', async () => {
      await build(report({ repeatReads: SERVED }));
      expect(said()).not.toMatch(/\bsav/i);
      expect(said()).not.toMatch(/[$%]/);
    });

    it('sits after the not-optimized block, below every total', async () => {
      await build(
        report({ currentChannel: 'cli', missedCalls: 3, missedFullTokens: 12_000, repeatReads: SERVED }),
      );
      const missed = fixture.nativeElement.querySelector('.missed') as HTMLElement;
      const card = fixture.nativeElement.querySelector(CARD) as HTMLElement;
      expect(missed.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });

    it('shows the figures when they arrive after the page opened', async () => {
      const o = await build();
      expect(fixture.nativeElement.querySelector(CARD)).toBeNull();
      o.s.optimizerStats.set(report({ repeatReads: SERVED }));
      fixture.detectChanges();
      expect(figures()).toHaveLength(4);
    });

    // ── the token figure ─────────────────────────────────────────────────────

    it('says how many answers the token figure covers when only some were measured', async () => {
      await build(report({ repeatReads: { ...SERVED, measuredAnswers: 1_000 } }));
      expect(figures()[2]).toBe(
        `Tokens not re-sent ${n(4_125_000)} ${BASELINE} `
        + `Covers the ${n(1_000)} of ${n(1_540)} answers whose token count was measured.`,
      );
    });

    it('claims no token figure when no answer has a measured count', async () => {
      await build(report({ repeatReads: repeats({ unchangedServed: 4 }) }));
      expect(figures()[2]).toBe(
        'Tokens not re-sent — None of these answers has a measured token count, so no figure is claimed.',
      );
    });

    // ── asked for again ──────────────────────────────────────────────────────

    it('shows "0 of m" when answers were served and none was asked for again', async () => {
      await build(
        report({ repeatReads: repeats({ unchangedServed: 5, tokensNotResent: 900, measuredAnswers: 5 }) }),
      );
      expect(figures()[3]).toBe(
        'Full result asked for again (fresh=true) 0 of 5 5 = 5 unchanged + 0 deltas + 0 asked again.',
      );
    });

    it('says "1 delta", not "1 deltas", when a single edit was served as one', async () => {
      await build(
        report({
          repeatReads: repeats({ unchangedServed: 2, deltasServed: 1, tokensNotResent: 90, measuredAnswers: 3 }),
        }),
      );
      expect(figures()[3]).toBe(
        'Full result asked for again (fresh=true) 0 of 3 3 = 2 unchanged + 1 delta + 0 asked again.',
      );
    });

    it('hides that figure when nothing was served, because there is nothing for it to be a share of', async () => {
      await build(report({ repeatReads: repeats({ freshRequests: 3 }) }));
      expect(said()).toContain(NONE_YET);
      expect(figures()).toEqual([]);
      expect(said()).not.toContain('fresh');
    });

    // ── reads Lumen could not place in a conversation ────────────────────────

    it('says how many reads got a full result for want of a conversation, and links to Setup', async () => {
      await build(report({ repeatReads: { ...SERVED, unidentifiedReads: 3 } }));
      expect(said()).toContain(
        '3 reads got a full result because Lumen could not tell which conversation they came from.',
      );
      expect(said()).toContain('Open Setup to run it again, then restart Claude Code.');
      const link = fixture.nativeElement.querySelector(`${CARD} a[href="/setup"]`) as HTMLAnchorElement;
      expect(link.textContent?.trim()).toBe('Open Setup');
    });

    it('says "1 read", not "1 reads", for a single one', async () => {
      await build(report({ repeatReads: { ...SERVED, unidentifiedReads: 1 } }));
      expect(said()).toContain(
        '1 read got a full result because Lumen could not tell which conversation it came from.',
      );
    });

    it('has no such line, and no link to Setup, when every read could be placed', async () => {
      await build(report({ repeatReads: SERVED }));
      expect(said()).not.toContain('could not tell which conversation');
      expect(fixture.nativeElement.querySelector(`${CARD} a`)).toBeNull();
    });

    it('shows that line beside the none-yet text when nothing was served but reads went unplaced', async () => {
      await build(report({ repeatReads: repeats({ unidentifiedReads: 4 }) }));
      expect(said()).toContain(NONE_YET);
      expect(said()).toContain('4 reads got a full result because Lumen could not tell');
      expect(fixture.nativeElement.querySelector(`${CARD} a[href="/setup"]`)).not.toBeNull();
    });

    // ── answers served, and the setting is off now ───────────────────────────

    it('labels the figures as what happened so far, and says repeat reads are off now, when the file turns them off', async () => {
      await build(report({ repeatReads: SERVED }), usage(), fileSays(FILE_OFF));
      expect(said()).toContain('Answered so far');
      expect(said()).toContain(OFF_FILE);
      expect(said()).toContain(ENV_CAVEAT);
      expect(figures()).toHaveLength(4);
    });

    it('says a settings file that could not be read is why repeat reads are off now', async () => {
      await build(report({ repeatReads: SERVED }), usage(), fileSays(CORRUPT));
      expect(said()).toContain(
        `Repeat reads are off now: the settings file ${SETTINGS_FILE} could not be read `
        + '(not JSON: expected value at line 1 column 1). '
        + 'The Repeat reads switch on the Context tab rewrites it.',
      );
      expect(said()).toContain(ENV_CAVEAT);
      expect(said()).not.toContain('the settings file says off');
    });

    // ── the setting is not known ─────────────────────────────────────────────

    it('says nothing about on or off until the setting has been read', async () => {
      const read = deferred<RepeatReadsSetting>();
      await build(report({ repeatReads: repeats() }), usage(), (b) => {
        b.getRepeatReadsSetting = () => read.promise;
      });
      expect(said()).toContain(NONE_YET);
      expect(said()).not.toMatch(/\b(on|off)\b/i);
      expect(said()).not.toContain('could not be read');
    });

    it('says it could not read the setting, and claims nothing about it, when the read fails', async () => {
      await build(report({ repeatReads: repeats() }), usage(), cannotRead);
      expect(said()).toContain(
        'The repeat reads setting could not be read, so nothing is said here about its current state. '
        + 'The Context tab shows why.',
      );
      expect(said()).not.toMatch(/\b(on|off)\b/i);
    });

    it('still shows the figures when the setting could not be read, with no claim about it', async () => {
      await build(report({ repeatReads: SERVED }), usage(), cannotRead);
      expect(figures()).toHaveLength(4);
      expect(said()).not.toContain('off now');
      expect(said()).not.toContain('the settings file says');
      expect(said()).not.toContain(ENV_CAVEAT);
    });

    // ── a backend that predates the figures ──────────────────────────────────

    it('renders nothing from a backend that sends no repeat reads figures', async () => {
      await build(report({ lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000 }));
      expect(fixture.nativeElement.querySelector(CARD)).toBeNull();
      expect(text().toLowerCase()).not.toContain('repeat');
      // The page is there; it is the card that is not.
      expect(rendered('.savings-card--caused .savings-card__num')).toBe('900');
    });

    it('renders nothing from such a backend even when the setting is off', async () => {
      await build(report(), usage(), fileSays(FILE_OFF));
      expect(fixture.nativeElement.querySelector(CARD)).toBeNull();
      expect(text().toLowerCase()).not.toContain('repeat');
    });
  });

  // ── identity hook prompt ───────────────────────────────────────────────────
  //
  // Repeat reads are promised here, in the card above, and an upgrade from 1.7.0 or earlier
  // leaves them unable to work until Setup is run once. Someone who opens the app on this tab
  // is told so here as well. Its own behaviour is pinned in the component's spec; these pin
  // that this page hosts it, and for whom.

  describe('identity hook prompt', () => {
    const PROMPT = 'identity-hook-prompt [role="status"]';
    const MISSING = { registered: false, servedElsewhere: false };

    /** An upgrader: Setup done, so nothing sends them to it, and the hook not registered. */
    const upgrader = (status: unknown = MISSING) => (b: FakeTauriBridge) => {
      b.responses.set('lumen_setup_needed', false);
      b.responses.set('lumen_identity_hook_status', status);
    };

    it('tells an upgrader whose settings lack the hook to run Setup, above the tabs', async () => {
      await build(report(), usage(), upgrader());
      expect(rendered(PROMPT)).toContain('Repeat reads are on, but not working yet');
      expect(rendered(`${PROMPT} button`)).toBe('Run Setup');
      const prompt = (fixture.nativeElement as HTMLElement).querySelector(PROMPT)!;
      const nav = (fixture.nativeElement as HTMLElement).querySelector('nav.tab-nav')!;
      expect(prompt.compareDocumentPosition(nav) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });

    it('is not there once the hook is registered', async () => {
      await build(report(), usage(), upgrader({ registered: true, servedElsewhere: false }));
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });

    it('is not there when repeat reads are off', async () => {
      await build(report(), usage(), (b) => {
        upgrader()(b);
        b.responses.set('get_repeat_reads_setting', setting({ on: false, source: 'file' }));
      });
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });

    it('is not there when Setup is still to run', async () => {
      await build(report(), usage(), (b) => {
        upgrader()(b);
        b.responses.set('lumen_setup_needed', true);
      });
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });
  });
});
