import { TestBed } from '@angular/core/testing';
import { SessionService, WINDOW_OPTIONS, modelWindow, resolveWindow } from './session.service';
import { TauriBridge } from './tauri-bridge';
import { FakeTauriBridge } from './tauri-bridge.fake';
import { RATE } from './components';
import type { OptimizerReport, UsageReport, TokenAgg } from './components';

/**
 * SessionService is the whole frontend data layer: it folds daemon frames into
 * per-session state, infers the context window, derives cost, and edge-triggers
 * the alerts. All of it now runs under test because every backend call goes
 * through TauriBridge.
 */
describe('SessionService', () => {
  let bridge: FakeTauriBridge;

  /** Construct the service with a fake bridge, optionally pre-seeded. */
  function build(seed?: (b: FakeTauriBridge) => void): SessionService {
    bridge = new FakeTauriBridge();
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }],
    });
    return TestBed.inject(SessionService);
  }

  function agg(over: Partial<TokenAgg> = {}): TokenAgg {
    return {
      turns: 0,
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      ...over,
    };
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

  function optimizer(over: Partial<OptimizerReport> = {}): OptimizerReport {
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

  /** A daemon "event" frame for one turn. */
  function turnFrame(over: Record<string, unknown> = {}): string {
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

  /**
   * Flush Angular effects and the microtask queue together.
   *
   * A native toast is not synchronous: the effect fires, then
   * fireNativeNotification awaits isPermissionGranted() (and possibly
   * requestPermission()) before sending. tick() alone only drains the effects,
   * so anything asserting on notifications must settle the promises too.
   */
  async function settle(rounds = 4): Promise<void> {
    for (let i = 0; i < rounds; i++) {
      TestBed.tick();
      await Promise.resolve();
    }
  }

  afterEach(() => {
    TestBed.resetTestingModule();
  });

  // ── construction ──────────────────────────────────────────────────────────

  it('is created without a Tauri runtime', () => {
    expect(build()).toBeTruthy();
  });

  it('asks the backend for the cached snapshot on construction', () => {
    build();
    expect(bridge.countOf('request_snapshot')).toBe(1);
  });

  it('starts with empty state before any frame arrives', () => {
    const s = build();
    expect(s.fill()).toBe(0);
    expect(s.model()).toBe('');
    expect(s.totals()).toEqual({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0 });
  });

  it('survives a backend that rejects every call', async () => {
    const s = build((b) => {
      b.failures.add('request_snapshot');
      b.failures.add('get_usage');
      b.failures.add('get_optimizer_stats');
    });
    await Promise.resolve();
    // Outside Tauri every invoke rejects; the service must degrade to zeros
    // rather than throw during construction.
    expect(s.fill()).toBe(0);
    expect(s.usage()).toBeNull();
  });

  // ── daemon frame folding ──────────────────────────────────────────────────

  it('folds a turn event into fill, model and totals', () => {
    const s = build();
    bridge.emit(
      'daemon',
      turnFrame({
        input_tokens: 10,
        output_tokens: 20,
        cache_read_input_tokens: 30_000,
        cache_creation_input_tokens: 40,
      }),
    );
    expect(s.fill()).toBe(30_000);
    expect(s.model()).toBe('claude-sonnet-4');
    expect(s.totals()).toEqual({
      input: 10,
      output: 20,
      cacheRead: 30_000,
      cacheWrite: 40,
    });
  });

  it('accumulates totals across successive turns', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ input_tokens: 5, output_tokens: 1 }));
    bridge.emit('daemon', turnFrame({ input_tokens: 7, output_tokens: 2 }));
    expect(s.totals().input).toBe(12);
    expect(s.totals().output).toBe(3);
  });

  it('takes fill from the latest turn rather than summing it', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 90_000 }));
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 50_000 }));
    expect(s.fill()).toBe(50_000);
  });

  it('applies a snapshot frame across several sessions', () => {
    const s = build();
    bridge.emit(
      'daemon',
      JSON.stringify({
        type: 'snapshot',
        sessions: [
          {
            session_id: 'older',
            fill: 1_000,
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            ts: '2026-01-01T10:00:00Z',
          },
          {
            session_id: 'newer',
            fill: 7_000,
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            ts: '2026-01-01T12:00:00Z',
          },
        ],
      }),
    );
    // The active session is the one with the newest ts.
    expect(s.fill()).toBe(7_000);
    expect(s.totals()).toEqual({ input: 10, output: 20, cacheRead: 30, cacheWrite: 40 });
  });

  it('ignores malformed and unknown daemon frames', () => {
    const s = build();
    bridge.emit('daemon', 'not json at all');
    bridge.emit('daemon', JSON.stringify({ type: 'something-else' }));
    bridge.emit('daemon', JSON.stringify({ type: 'snapshot' })); // no sessions array
    expect(s.fill()).toBe(0);
    expect(s.model()).toBe('');
  });

  it('keeps the previous model when a turn reports an empty one', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ model: 'claude-opus-4' }));
    bridge.emit('daemon', turnFrame({ model: '' }));
    expect(s.model()).toBe('claude-opus-4');
  });

  it('passes a nonsensical negative fill through without throwing', () => {
    // cache_read_input_tokens is never negative in practice; this only pins that
    // a bad frame degrades rather than crashes. Clamping to a sane 0-100% range
    // is the Gauge component's job (see gauge.spec.ts).
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: -5 }));
    expect(s.fill()).toBe(-5);
    expect(s.trayStatus()).toBe('ok');
  });

  // ── context window resolution ─────────────────────────────────────────────
  //
  // The window is a property of the MODEL. The tier-inference tests below use
  // `claude-sonnet-4`, which has no published entry, so they cover the FALLBACK
  // path; the model-driven tests that follow cover the normal path.

  it('reports the published window for a known model, whatever the fill', () => {
    // THE REPORTED BUG: "267,593 / 500,000" on a model whose window is 1M. The
    // window was inferred from the fill, and a 1M session that has only reached
    // 267K is indistinguishable from a 500K one.
    const s = build();
    bridge.emit('daemon', turnFrame({ model: 'claude-opus-4-8', cache_read_input_tokens: 267_593 }));
    expect(s.fill()).toBe(267_593);
    expect(s.maxContext()).toBe(1_000_000);
  });

  it.each([
    ['claude-opus-5', 1_000_000],
    ['claude-opus-4-8', 1_000_000],
    ['claude-opus-4-7', 1_000_000],
    ['claude-opus-4-6', 1_000_000],
    ['claude-sonnet-5', 1_000_000],
    ['claude-sonnet-4-6', 1_000_000],
    ['claude-haiku-4-5', 200_000],
  ])('gives %s a window of %i', (model, expected) => {
    const s = build();
    bridge.emit('daemon', turnFrame({ model, cache_read_input_tokens: 10_000 }));
    expect(s.maxContext()).toBe(expected);
  });

  it('resolves a dated model id like its alias', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ model: 'claude-haiku-4-5-20251001' }));
    expect(s.maxContext()).toBe(200_000);
  });

  it('keeps the window steady when a compaction drops the fill', () => {
    // Real pattern from a shipped DB: 317,241 -> 16,794 -> 314,173. Deriving the
    // window from the momentary fill collapsed it to 200K mid-session.
    const s = build();
    bridge.emit('daemon', turnFrame({ model: 'mystery-model', cache_read_input_tokens: 317_241 }));
    expect(s.maxContext()).toBe(500_000);

    bridge.emit('daemon', turnFrame({ model: 'mystery-model', cache_read_input_tokens: 16_794 }));
    expect(s.fill()).toBe(16_794);
    expect(s.peakFill()).toBe(317_241);
    expect(s.maxContext()).toBe(500_000);
  });

  it('tracks the peak fill monotonically', () => {
    const s = build();
    for (const fill of [1_000, 50_000, 20_000, 90_000, 10_000]) {
      bridge.emit('daemon', turnFrame({ cache_read_input_tokens: fill }));
    }
    expect(s.peakFill()).toBe(90_000);
    expect(s.fill()).toBe(10_000);
  });

  it('uses the peak and model the daemon reports in a snapshot', () => {
    const s = build();
    bridge.emit(
      'daemon',
      JSON.stringify({
        type: 'snapshot',
        sessions: [{
          session_id: 's1', fill: 12_000, peak_fill: 400_000,
          model: 'claude-opus-4-8', input: 0, output: 0, cache_read: 0, cache_write: 0,
          ts: '2026-01-01T10:00:00Z',
        }],
      }),
    );
    expect(s.fill()).toBe(12_000);
    expect(s.peakFill()).toBe(400_000);
    expect(s.maxContext()).toBe(1_000_000);
  });

  it('mirrors the Rust window table exactly', () => {
    // crates/lumen-core/src/rates.rs MODEL_WINDOWS is the other half of this
    // pair; a drift between them shows the same session two different windows
    // in the TUI and the GUI.
    expect(modelWindow('claude-opus-4-8')).toBe(1_000_000);
    expect(modelWindow('claude-haiku-4-5')).toBe(200_000);
    expect(modelWindow('claude-sonnet-4')).toBeNull();
    expect(modelWindow('')).toBeNull();
    expect(resolveWindow('', 0)).toBe(200_000);
    // Never claim a window narrower than a fill actually observed.
    expect(resolveWindow('claude-haiku-4-5', 400_000)).toBe(500_000);
  });

  it('infers the smallest tier that fits an unknown model (fallback path)', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 150_000 }));
    expect(s.maxContext()).toBe(200_000);
  });

  it('steps an unknown model up to the next tier as fill grows (fallback path)', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 250_000 }));
    expect(s.maxContext()).toBe(500_000);
  });

  it('clamps an unknown model to the largest tier (fallback path)', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 5_000_000 }));
    expect(s.maxContext()).toBe(1_000_000);
  });

  it('honours a manual window override over inference', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 10_000 }));
    s.setWindow(1_000_000);
    expect(s.maxContext()).toBe(1_000_000);
  });

  it('returns to inference when the override is cleared', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 10_000 }));
    s.setWindow(1_000_000);
    s.setWindow(null);
    expect(s.maxContext()).toBe(200_000);
  });

  it('exposes an Auto option plus every context tier', () => {
    expect(WINDOW_OPTIONS.map((o) => o.value)).toEqual([null, 200_000, 500_000, 1_000_000]);
  });

  it('puts the compaction threshold at 95% of the window', () => {
    const s = build();
    s.setWindow(200_000);
    expect(s.compactionThreshold()).toBe(190_000);
  });

  // ── tray percent / status thresholds ──────────────────────────────────────

  it('reports tray percent as a rounded share of the window', () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 50_000 }));
    expect(s.trayPercent()).toBe(25);
  });

  it.each([
    [0, 'ok'],
    [159_999, 'ok'],
    [160_000, 'warn'], // exactly 80%
    [189_999, 'warn'],
    [190_000, 'alert'], // exactly 95%
    [200_000, 'alert'],
  ])('maps a fill of %i in a 200K window to status %s', (fill, expected) => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: fill }));
    expect(s.trayStatus()).toBe(expected);
  });

  it('pushes the tray icon whenever fill or status changes', () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 190_000 }));
    TestBed.tick();
    expect(bridge.lastArgsOf('update_tray')).toEqual({ percent: 95, status: 'alert' });
  });

  // ── cost ──────────────────────────────────────────────────────────────────

  it('prices the session from the live totals via the RATE table', () => {
    const s = build();
    bridge.emit(
      'daemon',
      turnFrame({
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_read_input_tokens: 1_000_000,
        cache_creation_input_tokens: 1_000_000,
      }),
    );
    const expected =
      1_000_000 * RATE.input +
      1_000_000 * RATE.output +
      1_000_000 * RATE.cacheRead +
      1_000_000 * RATE.cacheWrite;
    expect(s.costSession()).toBeCloseTo(expected, 10);
  });

  it("reports today's cost as zero until the usage report loads", () => {
    const s = build();
    expect(s.costToday()).toBe(0);
  });

  it("prices today from the usage rollup once it loads", async () => {
    const s = build((b) =>
      b.responses.set('get_usage', usage({ today: agg({ input: 2_000_000 }) })),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(s.costToday()).toBeCloseTo(2_000_000 * RATE.input, 10);
  });

  // ── optimizer signals ─────────────────────────────────────────────────────

  it('reports zeros for every optimizer signal before the report loads', () => {
    const s = build();
    expect(s.lifetimeOptimizedTokens()).toBe(0);
    expect(s.lifetimeOptimizedUsd()).toBe(0);
    expect(s.optimizedToday()).toBe(0);
    expect(s.optimizedThisWeek()).toBe(0);
    expect(s.effectivenessPct()).toBe(0);
    expect(s.optimizedByChannel()).toEqual([]);
    expect(s.optimizedByTool()).toEqual([]);
    expect(s.currentChannel()).toBe('unknown');
    expect(s.missedReads()).toEqual({ calls: 0, fullTokens: 0 });
  });

  it('converts optimizer token savings at the input rate', async () => {
    const s = build((b) =>
      b.responses.set(
        'get_optimizer_stats',
        optimizer({ lifetimeOptimizedTokens: 4_000_000 }),
      ),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(s.lifetimeOptimizedUsd()).toBeCloseTo(4_000_000 * RATE.input, 10);
  });

  it('computes effectiveness as saved over full', async () => {
    const s = build((b) =>
      b.responses.set(
        'get_optimizer_stats',
        optimizer({ lifetimeOptimizedTokens: 750, lifetimeFullTokens: 1_000 }),
      ),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(s.effectivenessPct()).toBeCloseTo(75, 10);
  });

  it('reports zero effectiveness rather than dividing by zero', async () => {
    const s = build((b) =>
      b.responses.set(
        'get_optimizer_stats',
        optimizer({ lifetimeOptimizedTokens: 500, lifetimeFullTokens: 0 }),
      ),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(s.effectivenessPct()).toBe(0);
  });

  it('surfaces missed reads separately from savings', async () => {
    const s = build((b) =>
      b.responses.set(
        'get_optimizer_stats',
        optimizer({ missedCalls: 3, missedFullTokens: 9_000 }),
      ),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(s.missedReads()).toEqual({ calls: 3, fullTokens: 9_000 });
    expect(s.lifetimeOptimizedTokens()).toBe(0);
  });

  // ── spend limits ──────────────────────────────────────────────────────────

  it('defaults the spend limits to $5 daily and $2 per session', () => {
    const s = build();
    expect(s.dailySpendLimit()).toBe(5);
    expect(s.sessionSpendLimit()).toBe(2);
  });

  it('refuses a negative spend limit', () => {
    const s = build();
    s.setDailyLimit(-10);
    s.setSessionLimit(-1);
    expect(s.dailySpendLimit()).toBe(0);
    expect(s.sessionSpendLimit()).toBe(0);
  });

  // ── notification priority ─────────────────────────────────────────────────

  it('has no notification when everything is calm', () => {
    const s = build();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 1_000 }));
    expect(s.notification()).toBeNull();
  });

  it('raises a compaction alert at the 95% threshold', () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 190_000 }));
    const n = s.notification();
    expect(n?.level).toBe('alert');
    expect(n?.text).toContain('compaction imminent');
  });

  it('warns when context passes 80% but is not yet compacting', () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 170_000 }));
    const n = s.notification();
    expect(n?.level).toBe('warn');
    expect(n?.text).toContain('85%');
  });

  it('prioritises compaction over a daily cost alert', async () => {
    const s = build((b) =>
      b.responses.set('get_usage', usage({ today: agg({ output: 100_000_000 }) })),
    );
    await Promise.resolve();
    await Promise.resolve();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    TestBed.tick();
    // Both conditions hold; compaction is the more time-critical one.
    expect(s.notification()?.text).toContain('compaction imminent');
  });

  it('raises a daily cost alert above the daily limit', async () => {
    const s = build((b) =>
      b.responses.set('get_usage', usage({ today: agg({ output: 1_000_000 }) })),
    );
    await Promise.resolve();
    await Promise.resolve();
    s.setDailyLimit(1); // $25 of output spend against a $1 limit
    TestBed.tick();
    const n = s.notification();
    expect(n?.level).toBe('alert');
    expect(n?.text).toContain('daily limit');
  });

  it('clears the cost alert when spend drops back under the limit', async () => {
    const s = build((b) =>
      b.responses.set('get_usage', usage({ today: agg({ output: 1_000_000 }) })),
    );
    await Promise.resolve();
    await Promise.resolve();
    s.setDailyLimit(1);
    TestBed.tick();
    expect(s.notification()?.level).toBe('alert');

    s.setDailyLimit(1000); // now comfortably under
    TestBed.tick();
    expect(s.notification()).toBeNull();
  });

  it('treats a zero limit as "disabled", never as "always exceeded"', async () => {
    const s = build((b) =>
      b.responses.set('get_usage', usage({ today: agg({ output: 1_000_000 }) })),
    );
    await Promise.resolve();
    await Promise.resolve();
    s.setDailyLimit(0);
    s.setSessionLimit(0);
    TestBed.tick();
    expect(s.notification()).toBeNull();
  });

  it('warns about a high output rate once five turns are seen', () => {
    const s = build();
    s.setWindow(1_000_000); // keep context out of the way
    for (let i = 0; i < 5; i++) {
      bridge.emit('daemon', turnFrame({ output_tokens: 9_000, cache_read_input_tokens: 10 }));
    }
    const n = s.notification();
    expect(n?.level).toBe('warn');
    expect(n?.text).toContain('High output rate');
  });

  it('stays quiet about output rate with fewer than five turns', () => {
    const s = build();
    s.setWindow(1_000_000);
    for (let i = 0; i < 4; i++) {
      bridge.emit('daemon', turnFrame({ output_tokens: 9_000, cache_read_input_tokens: 10 }));
    }
    expect(s.notification()).toBeNull();
  });

  // ── native notifications ──────────────────────────────────────────────────

  it('sends a native toast once per compaction crossing, not once per change', async () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 196_000 }));
    await settle();

    const compaction = bridge.notifications.filter((n) => n.title.includes('Context Alert'));
    expect(compaction).toHaveLength(1);
  });

  it('re-arms the compaction toast after dropping back below the threshold', async () => {
    const s = build();
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 1_000 })); // fresh context
    await settle();
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();

    expect(bridge.notifications.filter((n) => n.title.includes('Context Alert'))).toHaveLength(2);
  });

  it('sends no native toast while notifications are disabled', async () => {
    const s = build();
    s.setNativeNotify(false);
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();
    expect(bridge.notifications).toHaveLength(0);
  });

  it('requests permission when it has not been granted', async () => {
    const s = build((b) => {
      b.permissionGranted = false;
      b.permissionAnswer = 'granted';
    });
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();
    // Permission was refused up front but granted on request, so the toast lands.
    expect(bridge.notifications.length).toBeGreaterThan(0);
  });

  it('stays silent when permission is denied', async () => {
    const s = build((b) => {
      b.permissionGranted = false;
      b.permissionAnswer = 'denied';
    });
    s.setWindow(200_000);
    bridge.emit('daemon', turnFrame({ cache_read_input_tokens: 195_000 }));
    await settle();
    expect(bridge.notifications).toHaveLength(0);
  });

  // ── refresh plumbing ──────────────────────────────────────────────────────

  it('fetches both aggregate reports on construction', () => {
    build();
    expect(bridge.countOf('get_usage')).toBe(1);
    expect(bridge.countOf('get_optimizer_stats')).toBe(1);
  });

  it('re-fetches on demand', async () => {
    const s = build((b) => {
      b.responses.set('get_usage', usage());
      b.responses.set('get_optimizer_stats', optimizer());
    });
    s.refreshUsage();
    s.refreshOptimizerStats();
    await Promise.resolve();
    expect(bridge.countOf('get_usage')).toBe(2);
    expect(bridge.countOf('get_optimizer_stats')).toBe(2);
  });
});
