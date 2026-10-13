import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Details } from './details';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { OptimizerReport } from '../../components/index';

/**
 * Details holds what Home and the Optimizer screen gave up in 1.9.0: Claude Code's context
 * window, and the Optimizer's own workings. Each test reads the rendered page, since what moved
 * is what the user sees.
 */
describe('Details', () => {
  let fixture: ComponentFixture<Details>;
  let bridge: FakeTauriBridge;

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

  async function build(r: OptimizerReport = report()): Promise<Details> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_optimizer_stats', r);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Details);
    fixture.detectChanges();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function el<T extends Element>(selector: string): T {
    const found = (fixture.nativeElement as HTMLElement).querySelector<T>(selector);
    expect(found, selector).not.toBeNull();
    return found!;
  }

  /** What one element renders, whitespace collapsed. */
  function rendered(selector: string): string {
    return (el(selector).textContent ?? '').replace(/\s+/g, ' ').trim();
  }

  function text(): string {
    return ((fixture.nativeElement as HTMLElement).textContent ?? '').replace(/\s+/g, ' ');
  }

  function turn(over: Record<string, unknown>): string {
    return JSON.stringify({
      type: 'event',
      turn: {
        session_id: 's1',
        model: 'claude-sonnet-4',
        input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        ...over,
      },
    });
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── the context window (from Home) ─────────────────────────────────────────

  describe('context window', () => {
    it('shows the gauge and the window selector, with Auto chosen', async () => {
      const d = await build();
      expect(fixture.nativeElement.querySelector('gauge')).not.toBeNull();
      const tabs = Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.segmented__opt'),
      );
      expect(tabs.map((t) => t.textContent?.trim())).toEqual(['Auto', '200K', '500K', '1M']);
      expect(d.activeIndex()).toBe(0);
    });

    it('sets the window tier from the segmented control', async () => {
      const d = await build();
      const tabs = (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.segmented__opt');
      tabs[2].click();
      fixture.detectChanges();
      expect(d.s.contextOverride()).toBe(500_000);
      expect(d.activeIndex()).toBe(2);
      expect(tabs[2].getAttribute('aria-selected')).toBe('true');
      expect(tabs[0].getAttribute('aria-selected')).toBe('false');
    });

    it('falls back to Auto for an unknown override value', async () => {
      const d = await build();
      d.s.setWindow(123_456);
      expect(d.activeIndex()).toBe(0);
    });

    it('reflects a live daemon frame in the rendered gauge', async () => {
      const d = await build();
      bridge.emit('daemon', turn({ cache_read_input_tokens: 100_000 }));
      fixture.detectChanges();
      expect(d.s.fill()).toBe(100_000);
      expect(text()).toContain('claude-sonnet-4');
    });

    it('shows the active alert as a banner at its level', async () => {
      await build();
      bridge.emit('daemon', turn({ cache_read_input_tokens: 195_000 }));
      fixture.detectChanges();
      const banner = el<HTMLElement>('.banner');
      expect(banner.getAttribute('data-level')).toBe('alert');
      expect(banner.textContent).toContain('compaction imminent');
    });

    it('shows no banner while nothing is wrong', async () => {
      await build();
      bridge.emit('daemon', turn({ cache_read_input_tokens: 10_000 }));
      fixture.detectChanges();
      expect(fixture.nativeElement.querySelector('.banner')).toBeNull();
    });

    it('names the project the gauge follows, without a count for one session', async () => {
      const d = await build();
      bridge.emit('daemon', turn({ project: 'lumen' }));
      fixture.detectChanges();
      expect(rendered('.gauge-stage__project')).toBe('lumen');
      expect(fixture.nativeElement.querySelector('.gauge-stage__count')).toBeNull();
      expect(d.projectHint()).toBe('Project: lumen');
    });

    it('says how many other sessions exist when it follows one of several', async () => {
      const d = await build();
      bridge.emit('daemon', turn({ project: 'lumen' }));
      bridge.emit('daemon', turn({ session_id: 's2', project: 'speedash' }));
      fixture.detectChanges();
      expect(rendered('.gauge-stage__count')).toBe('+1 more');
      expect(d.projectHint()).toContain('most recently active of 2 sessions: speedash');
      expect(d.projectHint()).toContain('spend figures on Settings follow the same session');
    });
  });

  // ── the optimizer's workings (from the Optimizer screen) ───────────────────

  describe('optimizer', () => {
    it('says there is nothing yet, with no ratio and no counts, before any optimized read', async () => {
      const d = await build();
      expect(d.hasData()).toBe(false);
      expect(rendered('.details__empty')).toBe('No optimizer events recorded yet.');
      expect(fixture.nativeElement.querySelector('.effect')).toBeNull();
      expect(fixture.nativeElement.querySelector('.provenance')).toBeNull();
    });

    it('shows the token ratio, rounded to one decimal', async () => {
      const d = await build(report({ lifetimeOptimizedTokens: 6_666, lifetimeFullTokens: 10_000 }));
      expect(d.effectPct()).toBe(66.7);
      expect(rendered('.effect')).toBe(
        'Intercepted reads returned 66.7% fewer tokens than the files they read, across every one recorded.',
      );
    });

    it('never shows NaN or a negative ratio', async () => {
      const d = await build(report({ lifetimeOptimizedTokens: 500, lifetimeFullTokens: 0 }));
      expect(d.effectPct()).toBe(0);
      expect(text()).not.toContain('NaN');
    });

    it('shows the provenance counts, with no caveat when every count was measured', async () => {
      await build(report({
        lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000,
        unverifiedProvenanceRows: 0, provenanceTotalRows: 7_192,
      }));
      const rows = Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll('.provenance__row'),
      ).map((r) => `${r.querySelector('dt')?.textContent?.replace(/[ⓘ\s]+/g, ' ').trim()} `
        + `${r.querySelector('dd')?.textContent?.trim()}`);
      expect(rows).toEqual([
        `Metered events ${(7_192).toLocaleString()}`,
        'With unverified token provenance 0',
      ]);
      expect(fixture.nativeElement.querySelector('.provenance__warn')).toBeNull();
    });

    it('says N of M unverified when some counts were not measured', async () => {
      await build(report({
        lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000,
        unverifiedProvenanceRows: 2_584, provenanceTotalRows: 7_192,
      }));
      expect(rendered('.provenance__warn')).toContain(
        `${(2_584).toLocaleString()} of ${(7_192).toLocaleString()} metered events have `
        + 'unverified token provenance',
      );
    });

    it('never claims the counts were measured to the token: that sentence is the Optimizer screen\'s', async () => {
      await build(report({ lifetimeOptimizedTokens: 900, lifetimeFullTokens: 1_000 }));
      expect(text()).not.toContain('never estimated');
    });
  });

  // ── breakdowns (from the Optimizer screen) ─────────────────────────────────

  describe('breakdowns', () => {
    function rows(section: string): string[] {
      return Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll(`section[aria-label="${section}"] .breakdown__row`),
      ).map((r) => (r.textContent ?? '').replace(/\s+/g, ' ').trim());
    }

    it('renders a row per tool, without the mcp prefix', async () => {
      await build(report({
        lifetimeOptimizedTokens: 1_500,
        lifetimeFullTokens: 3_000,
        byTool: [
          { tool: 'mcp__lumen__smart_read', calls: 2, savedTokens: 1_000, fullTokens: 2_000 },
          { tool: 'mcp__lumen__recall_file', calls: 1, savedTokens: 400, fullTokens: 800 },
          { tool: 'mcp__lumen__compress_logs', calls: 1, savedTokens: 100, fullTokens: 200 },
          { tool: 'something_new', calls: 1, savedTokens: 0, fullTokens: 0 },
        ],
      }));
      expect(rows('Savings by tool')).toEqual([
        `smart_read ${(1_000).toLocaleString()} tok · 2 calls`,
        'recall_file 400 tok · 1 call',
        'compress_logs 100 tok · 1 call',
        'something_new 0 tok · 1 call',
      ]);
    });

    it('renders a row per channel, labelled with its mode', async () => {
      await build(report({
        lifetimeOptimizedTokens: 1_500,
        lifetimeFullTokens: 3_000,
        byChannel: [
          { channel: 'cli', calls: 2, savedTokens: 1_000, fullTokens: 2_000 },
          { channel: 'vscode', calls: 1, savedTokens: 500, fullTokens: 1_000 },
          { channel: 'unknown', calls: 1, savedTokens: 0, fullTokens: 0 },
        ],
      }));
      expect(rows('Savings by channel')).toEqual([
        `CLI (Full mode) ${(1_000).toLocaleString()} tok · 2 calls`,
        'VS Code (Soft mode) 500 tok · 1 call',
        'unknown 0 tok · 1 call',
      ]);
    });

    it('renders no breakdown until a row exists', async () => {
      await build();
      expect(fixture.nativeElement.querySelector('section[aria-label="Savings by tool"]')).toBeNull();
      expect(fixture.nativeElement.querySelector('section[aria-label="Savings by channel"]')).toBeNull();
    });

    it('scales bars against the largest row', async () => {
      const d = await build(report({
        byTool: [
          { tool: 'smart_read', calls: 1, savedTokens: 1_000, fullTokens: 2_000 },
          { tool: 'recall_file', calls: 1, savedTokens: 500, fullTokens: 1_000 },
        ],
      }));
      expect(d.maxToolSaved()).toBe(1_000);
      expect(d.barPct(1_000, d.maxToolSaved())).toBe(100);
      expect(d.barPct(500, d.maxToolSaved())).toBe(50);
    });

    it('never divides by zero', async () => {
      const d = await build(
        report({ byTool: [{ tool: 'smart_read', calls: 1, savedTokens: 0, fullTokens: 0 }] }),
      );
      expect(d.maxToolSaved()).toBe(1);
      expect(d.maxChanSaved()).toBe(1);
      expect(d.barPct(0, d.maxToolSaved())).toBe(0);
      expect(d.barPct(5, 0)).toBe(0);
    });
  });
});
