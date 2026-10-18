import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Home } from './home';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { APP_VERSION_CMD, FakeTauriBridge } from '../../tauri-bridge.fake';
import type { RepeatReadsSetting } from '../../components/index';
import type { BillingModeSetting } from '../../billing-mode.service';
import type { SavingsSummary } from '../../components/savings-headline/savings-headline';
import { recordedPeriod, savingsSummary } from '../../savings-summary.fake';

describe('Home', () => {
  let fixture: ComponentFixture<Home>;
  let bridge: FakeTauriBridge;

  const SETTINGS_FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: SETTINGS_FILE, ...over };
  }

  function billing(over: Partial<BillingModeSetting> = {}): BillingModeSetting {
    return { mode: 'subscription', problem: null, file: SETTINGS_FILE, ...over };
  }

  async function build(seed?: (b: FakeTauriBridge) => void): Promise<Home> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_setup_needed', false);
    // No settings file: repeat reads are on by default.
    bridge.responses.set('get_repeat_reads_setting', setting());
    // Answered already, so the billing question stays out of tests about something else.
    bridge.responses.set('get_billing_mode_setting', billing());
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: TauriBridge, useValue: bridge },
        SessionService,
      ],
    });
    fixture = TestBed.createComponent(Home);
    fixture.detectChanges();
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  function el<T extends Element>(selector: string): T {
    const found = (fixture.nativeElement as HTMLElement).querySelector<T>(selector);
    expect(found, selector).not.toBeNull();
    return found!;
  }

  function text(): string {
    return ((fixture.nativeElement as HTMLElement).textContent ?? '').replace(/\s+/g, ' ');
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── first-run redirect ─────────────────────────────────────────────────────

  it('asks the backend whether setup is needed', async () => {
    await build();
    // Once for the redirect, and once for the identity hook prompt on this page, which has to
    // know it as well and asks wherever it is hosted.
    expect(bridge.countOf('lumen_setup_needed')).toBe(2);
  });

  it('stays on home when setup is already done', async () => {
    TestBed.configureTestingModule({});
    const h = await build((b) => b.responses.set('lumen_setup_needed', false));
    const spy = vi.spyOn(TestBed.inject(Router), 'navigate');
    expect(h).toBeTruthy();
    expect(spy).not.toHaveBeenCalled();
  });

  it('proceeds normally when the backend cannot answer', async () => {
    // Outside Tauri the command rejects; that must not block the dashboard.
    const h = await build((b) => b.failures.add('lumen_setup_needed'));
    expect(h).toBeTruthy();
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders without a Tauri runtime', async () => {
    await build();
    expect(text().length).toBeGreaterThan(0);
  });

  // ── degraded-startup banner ────────────────────────────────────────────────
  //
  // Issue #5: the tray never appeared, nothing errored, and the app presented itself as
  // completely healthy. The user had no way to know what was wrong, or that filing a report
  // was worth doing. These tests pin both halves — it shows when something is wrong, and it
  // stays out of the way when nothing is.

  it('shows nothing when startup was healthy', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'present', degradations: [],
    }));
    expect(fixture.nativeElement.querySelector('.degraded')).toBeNull();
  });

  it('shows nothing when the backend has no such command', async () => {
    // An older backend simply rejects the call; that must not blank the screen or throw.
    await build();
    expect(fixture.nativeElement.querySelector('.degraded')).toBeNull();
  });

  it('reports an invisible tray even when nothing errored', async () => {
    // The exact issue #5 shape: no degradations at all, yet the app is unusable.
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: true,
      tray: 'built but not visible: hidden by preference',
      degradations: [],
    }));
    const el = fixture.nativeElement.querySelector('.degraded');
    expect(el).not.toBeNull();
    expect(el.textContent).toContain('not visible');
    expect(el.textContent).toContain('Report a fault');
    // Where Report a fault is now. It was on Hotspots, though this said Optimizer.
    expect(el.textContent).toContain('on the Settings screen');
  });

  it('lists every degradation it was given', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: true,
      tray: 'present',
      degradations: ['daemon: could not spawn: ENOENT', 'tray-menu: could not build the menu'],
    }));
    const items = fixture.nativeElement.querySelectorAll('.degraded__item');
    expect(items.length).toBe(2);
    expect(items[0].textContent).toContain('could not spawn');
    expect(items[1].textContent).toContain('could not build the menu');
  });

  // ── restored-icon notice ───────────────────────────────────────────────────
  //
  // The launch that clears a hidden-icon preference opens this window. Without a word here the
  // icon coming back reads as the app overriding the user, and whoever ⌘-dragged it away on
  // purpose is never told how to stop Lumen.

  it('explains a restored icon on the launch that restored it', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'present', degradations: [],
      restored: ['NSStatusItem Visible Item-0'],
    }));
    const el = fixture.nativeElement.querySelector('.restored');
    expect(el).not.toBeNull();
    expect(el.textContent).toContain('hidden by macOS');
    expect(el.querySelector('strong')?.textContent).toBe('Quit Lumen');
    // The repair worked, so it is not presented as a fault.
    expect(fixture.nativeElement.querySelector('.degraded')).toBeNull();
  });

  it('says nothing about the icon when nothing was restored', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'present', degradations: [], restored: [],
    }));
    expect(fixture.nativeElement.querySelector('.restored')).toBeNull();
  });

  it('says nothing about the icon when the backend predates the field', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'present', degradations: [],
    }));
    expect(fixture.nativeElement.querySelector('.restored')).toBeNull();
  });

  it('shows both when a restored icon is still not visible', async () => {
    // Clearing the preference cannot make room in a full menu bar; the banner says that part.
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: true, tray: 'built but not visible: off-screen', degradations: [],
      restored: ['NSStatusItem Visible Item-0'],
    }));
    expect(fixture.nativeElement.querySelector('.degraded')).not.toBeNull();
    expect(fixture.nativeElement.querySelector('.restored')).not.toBeNull();
  });

  it('asks the backend for startup health once when nothing changes', async () => {
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'present', degradations: [],
    }));
    expect(bridge.countOf('lumen_startup_health')).toBe(1);
  });

  it('says why when the window is opened after it loaded', async () => {
    // The tray checks give up six seconds after launch, long after this page read a healthy
    // startup; the window they open must not be blank about why.
    await build(b => b.responses.set('lumen_startup_health', {
      degraded: false, tray: 'unknown (not yet verified)', degradations: [],
    }));
    expect(fixture.nativeElement.querySelector('.degraded')).toBeNull();

    bridge.responses.set('lumen_startup_health', {
      degraded: true, tray: 'built but not visible: it was created but is not visible', degradations: [],
    });
    bridge.emit('startup-health', 'the menu-bar icon is not visible: it was created but is not visible');
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();

    expect(bridge.countOf('lumen_startup_health')).toBe(2);
    expect(fixture.nativeElement.querySelector('.degraded__tray')?.textContent).toContain('built but not visible');
  });

  // ── first run ──────────────────────────────────────────────────────────────

  it('sends a first run to setup', async () => {
    // Spied on the prototype: the redirect is decided in ngOnInit, before a test could
    // reach the injected router, and an unmatched route would otherwise reject.
    const navigate = vi.spyOn(Router.prototype, 'navigate').mockResolvedValue(true);
    try {
      await build((b) => b.responses.set('lumen_setup_needed', true));
      expect(navigate).toHaveBeenCalledWith(['/setup']);
    } finally {
      navigate.mockRestore();
    }
  });

  // ── the version ────────────────────────────────────────────────────────────
  //
  // The version was only ever in a startup log line, so a user filing a report could not say
  // which build they had. It is read from the Tauri runtime, never written in the page: these
  // use a version no release has, so a literal in the template or the component would fail them.
  // The About section that repeats it is on Settings, and its tests are there.
  // That the runtime's answer is the one tauri.conf.json and Cargo.toml declare is the Rust
  // test `the_version_the_ui_shows_is_the_one_tauri_conf_and_every_cargo_toml_declare`, and the
  // built app's text is checked by lumenator/e2e.

  const SEEDED = '9.8.7';

  it('shows the version the runtime returned in the header', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, SEEDED));
    expect(el<HTMLElement>('.home__version').textContent).toBe(`Lumen ${SEEDED}`);
    // Inside the header, with the brand, as one text node.
    expect(el<HTMLElement>('.home__head .home__version').childNodes.length).toBe(1);
  });

  it('follows whatever version the runtime returns', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, '0.0.1-rc.2'));
    expect(el<HTMLElement>('.home__version').textContent).toBe('Lumen 0.0.1-rc.2');
  });

  it('asks the runtime for the version once', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, SEEDED));
    expect(bridge.countOf(APP_VERSION_CMD)).toBe(1);
  });

  it('shows no version in the header until the runtime has given one', async () => {
    // Held open: a placeholder would be a version the user did not install.
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_setup_needed', false);
    vi.spyOn(bridge, 'getVersion').mockReturnValue(new Promise<string>(() => {}));
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Home);
    fixture.detectChanges();
    await Promise.resolve();
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.home__version')).toBeNull();
  });

  it('shows no version line when the runtime cannot give it', async () => {
    const h = await build(b => b.failures.add(APP_VERSION_CMD));
    expect(h.version()).toBeNull();
    expect(fixture.nativeElement.querySelector('.home__version')).toBeNull();
  });

  // ── what Home holds since 1.9.0 ─────────────────────────────────────────────
  //
  // Home answers how much Lumen saved, whether it is working and what to do. The gauge and its
  // window selector went to Details with the alert banner; the spend figures, the thresholds,
  // the Repeat reads switch and About went to Settings. Each of these is asserted absent on
  // the rendered page with everything that used to make it appear present in the data.

  describe('answers three questions only', () => {
    it('shows no context gauge, window selector, alert banner, spend, thresholds, repeat reads switch or About', async () => {
      await build((b) => {
        const zero = { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
        b.responses.set('get_usage', {
          rolling5h: zero, windowStart: null, resetApprox: null,
          rolling7dOpus: zero, rolling7dOther: zero, today: zero, thisWeek: zero, allTime: zero,
        });
      });
      bridge.emit('daemon', JSON.stringify({ type: 'event', turn: {
        session_id: 's1', model: 'claude-sonnet-4', input_tokens: 0, output_tokens: 0,
        cache_read_input_tokens: 195_000, cache_creation_input_tokens: 0, project: 'lumen' } }));
      await settle();
      const root = fixture.nativeElement as HTMLElement;
      const present = ['gauge', '.segmented', '.banner', '.gauge-stage', 'cost', 'usage',
        'section.limits', 'section[aria-label="Repeat reads"]', 'section.about']
        .filter((sel) => root.querySelector(sel) !== null);
      expect(present).toEqual([]);
      const said = ['Alert Thresholds', 'Daily spend', 'Session cost', 'Usage & Cost',
        'compaction imminent', 'Native notifications'].filter((w) => text().includes(w));
      expect(said).toEqual([]);
    });

    it('puts the savings headline first under the navigation, and the status under it, last', async () => {
      await build();
      const nav = el<HTMLElement>('nav.tab-nav');
      const headline = el<HTMLElement>('lumen-savings-headline');
      expect(nav.compareDocumentPosition(headline) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(headline.nextElementSibling?.tagName).toBe('LUMEN-SAVINGS-STATUS');
      expect(headline.nextElementSibling?.nextElementSibling).toBeNull();
    });

    it('claims no saving before the summary has come: the headline renders nothing', async () => {
      const h = await build((b) => b.hangs.add('lumen_savings_summary'));
      expect(h.savings()).toBeNull();
      expect(el<HTMLElement>('lumen-savings-headline').childElementCount).toBe(0);
      expect(el<HTMLElement>('lumen-savings-status').childElementCount).toBe(0);
      expect(text()).not.toMatch(/sav(ed|ing)/i);
      expect(text()).not.toContain('$');
    });

    it('shows the summary the engine sends: bold figures, the status, and what to do', async () => {
      await build((b) => b.responses.set('lumen_savings_summary', savingsSummary({
        billingMode: 'api',
        week: recordedPeriod(1_200, 0.5),
        allTime: recordedPeriod(-400, -1.25),
        status: { state: 'notWorking', reason: 'Lumen cost more than it saved: net loss of at most $1.25 this week' },
        recommendation: { code: 'smallSavings', text: 'Send only larger files.' },
      })));
      await settle();
      const strong = Array.from(fixture.nativeElement.querySelectorAll('lumen-savings-headline strong'))
        .map((e) => (e as HTMLElement).textContent?.replace(/\s/g, ' '));
      expect(strong).toEqual(['≈ 1,200', '$0.50', '≈ 400', '$1.25']);
      expect(el<HTMLElement>('lumen-savings-headline').textContent).toContain('net loss of at most');
      expect(el<HTMLElement>('.status__line').textContent?.replace(/\s+/g, ' ').trim())
        .toBe('Status: not working: Lumen cost more than it saved: net loss of at most $1.25 this week');
      expect(el<HTMLElement>('.status__rec-text').textContent).toBe('Send only larger files.');
    });

    it('a failed first load says so, and shows no figure and no status', async () => {
      await build((b) => b.failures.add('lumen_savings_summary'));
      await settle();
      expect(el<HTMLElement>('lumen-savings-headline').textContent?.replace(/\s+/g, ' ').trim())
        .toBe("Lumen's savings could not be read: fake: lumen_savings_summary failed.");
      expect(fixture.nativeElement.querySelector('lumen-savings-headline strong')).toBeNull();
      expect(el<HTMLElement>('lumen-savings-status').childElementCount).toBe(0);
    });

    it('a failed refresh keeps the last good figures on screen, marked as not up to date', async () => {
      await build((b) => b.responses.set('lumen_savings_summary', savingsSummary({
        week: recordedPeriod(1_200, 0.5), allTime: recordedPeriod(5_000, 2), status: { state: 'working' },
      })));
      await settle();
      bridge.failures.add('lumen_savings_summary');
      window.dispatchEvent(new Event('focus'));
      await settle();
      expect(el<HTMLElement>('.headline__stale').textContent?.replace(/\s+/g, ' ').trim()).toBe(
        'Not up to date: the latest refresh failed (fake: lumen_savings_summary failed). These figures are as of 2026-10-14 12:00:00 UTC.',
      );
      const strong = Array.from(fixture.nativeElement.querySelectorAll('lumen-savings-headline strong'))
        .map((e) => (e as HTMLElement).textContent?.replace(/\s/g, ' '));
      expect(strong).toEqual(['≈ 1,200', '$0.50', '≈ 5,000', '$2.00']);
    });

    it('fetches the summary again once a billing choice is saved, since it changes how dollars are said', async () => {
      await build((b) => {
        b.responses.set('get_billing_mode_setting', billing({ mode: null }));
        b.responses.set('set_billing_mode_setting', billing({ mode: 'api' }));
      });
      await settle();
      const before = bridge.countOf('lumen_savings_summary');
      const api = Array.from(el<HTMLElement>('lumen-billing-prompt section').querySelectorAll('button'))
        .find((b) => b.textContent?.includes('API'))!;
      api.click();
      await settle();
      expect(bridge.countOf('lumen_savings_summary')).toBe(before + 1);
    });
  });

  // ── the billing question ─────────────────────────────────────────────────────
  //
  // Asked once, on Home, after Setup: how the user pays for Claude Code. Its own behaviour is
  // pinned in the component's spec; these pin that Home asks it, when, and where.

  describe('billing question', () => {
    const PROMPT = 'lumen-billing-prompt section';
    const UNASKED = billing({ mode: null });

    async function open(seed?: (b: FakeTauriBridge) => void): Promise<void> {
      await build((b) => {
        b.responses.set('get_billing_mode_setting', UNASKED);
        seed?.(b);
      });
      await settle();
    }

    it('asks how the user pays when the settings file has no answer, above the tabs', async () => {
      await open();
      const prompt = el<HTMLElement>(PROMPT);
      expect(prompt.textContent).toContain('How do you pay for Claude Code?');
      const buttons = Array.from(prompt.querySelectorAll('button')).map((b) => b.textContent?.trim());
      expect(buttons).toEqual(['Subscription (Pro/Max)', 'API (pay per token)']);
      expect(prompt.compareDocumentPosition(el('nav.tab-nav')) & Node.DOCUMENT_POSITION_FOLLOWING)
        .toBeTruthy();
    });

    it('is gone once a choice is made, and the choice is what was written', async () => {
      await open((b) => b.responses.set('set_billing_mode_setting', billing({ mode: 'api' })));
      const api = Array.from(el<HTMLElement>(PROMPT).querySelectorAll('button'))
        .find((b) => b.textContent?.includes('API'))!;
      api.click();
      await settle();
      expect(bridge.lastArgsOf('set_billing_mode_setting')).toEqual({ mode: 'api' });
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
      // Said, and focused, where the prompt was.
      const said = el<HTMLElement>('lumen-billing-prompt [role="status"]');
      expect(said.textContent?.trim())
        .toBe('Billing mode saved: API (pay per token). You can change it in Settings.');
      expect(document.activeElement).toBe(said);
    });

    it('is not asked again once the settings file holds an answer', async () => {
      await build();
      await settle();
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
      expect(bridge.countOf('get_billing_mode_setting')).toBe(1);
    });

    it('is not asked on a first run, which is on its way to Setup', async () => {
      const navigate = vi.spyOn(Router.prototype, 'navigate').mockResolvedValue(true);
      try {
        await open((b) => b.responses.set('lumen_setup_needed', true));
        expect(fixture.nativeElement.querySelector('lumen-billing-prompt')).toBeNull();
        expect(bridge.countOf('get_billing_mode_setting')).toBe(0);
      } finally {
        navigate.mockRestore();
      }
    });

    it('is not asked when the backend cannot say whether Setup is done', async () => {
      await open((b) => b.failures.add('lumen_setup_needed'));
      expect(fixture.nativeElement.querySelector('lumen-billing-prompt')).toBeNull();
    });
  });

  // ── identity hook prompt ───────────────────────────────────────────────────
  //
  // Anyone who ran Setup under 1.7.0 or earlier has its marker, so this page never sends them
  // back to it, and their settings lack the one hook repeat reads need. The prompt is the only
  // place the app says so. Its own behaviour is pinned in the component's spec; these pin that
  // Home hosts it, where, and for whom.

  describe('identity hook prompt', () => {
    const PROMPT = 'identity-hook-prompt [role="status"]';
    const MISSING = { registered: false, servedElsewhere: false };

    async function open(
      status: unknown,
      seed?: (b: FakeTauriBridge) => void,
    ): Promise<void> {
      await build((b) => {
        b.responses.set('lumen_identity_hook_status', status);
        seed?.(b);
      });
      for (let i = 0; i < 10; i++) await Promise.resolve();
      fixture.detectChanges();
    }

    it('tells an upgrader whose settings lack the hook to run Setup, above the tabs', async () => {
      await open(MISSING);
      expect(el<HTMLElement>(PROMPT).textContent).toContain('Repeat reads are on, but not working yet');
      expect(el<HTMLButtonElement>(`${PROMPT} button`).textContent?.trim()).toBe('Run Setup');
      // Where the banners are: before the tabs, so it is seen before the page is used.
      const before = el<HTMLElement>(PROMPT).compareDocumentPosition(el<HTMLElement>('nav.tab-nav'));
      expect(before & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });

    it('is not there once the hook is registered', async () => {
      await open({ registered: true, servedElsewhere: false });
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });

    it('is not there when the backend has no such command', async () => {
      await open(MISSING, (b) => b.failures.add('lumen_identity_hook_status'));
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });

    it('is not there when repeat reads are off', async () => {
      await open(MISSING, (b) =>
        b.responses.set('get_repeat_reads_setting', setting({ on: false, source: 'file' })),
      );
      expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
    });

    it('is not there on a first run, which is on its way to Setup anyway', async () => {
      const navigate = vi.spyOn(Router.prototype, 'navigate').mockResolvedValue(true);
      try {
        await open(MISSING, (b) => b.responses.set('lumen_setup_needed', true));
        expect(fixture.nativeElement.querySelector(PROMPT)).toBeNull();
      } finally {
        navigate.mockRestore();
      }
    });
  });
});
