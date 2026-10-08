import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Home } from './home';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';

describe('Home', () => {
  let fixture: ComponentFixture<Home>;
  let bridge: FakeTauriBridge;

  async function build(seed?: (b: FakeTauriBridge) => void): Promise<Home> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_setup_needed', false);
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

  afterEach(() => TestBed.resetTestingModule());

  // ── first-run redirect ─────────────────────────────────────────────────────

  it('asks the backend whether setup is needed', async () => {
    await build();
    expect(bridge.countOf('lumen_setup_needed')).toBe(1);
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

  // ── window selector ────────────────────────────────────────────────────────

  it('highlights Auto when no override is set', async () => {
    const h = await build();
    expect(h.activeIndex()).toBe(0);
  });

  it('highlights the chosen window tier', async () => {
    const h = await build();
    h.s.setWindow(500_000);
    // WINDOW_OPTIONS is [Auto, 200K, 500K, 1M].
    expect(h.activeIndex()).toBe(2);
  });

  it('falls back to Auto for an unknown override value', async () => {
    const h = await build();
    h.s.setWindow(123_456); // not one of the offered tiers
    expect(h.activeIndex()).toBe(0);
  });

  // ── spend-limit inputs ─────────────────────────────────────────────────────

  function inputEvent(value: string): Event {
    const el = document.createElement('input');
    el.value = value;
    return { target: el } as unknown as Event;
  }

  it('applies a daily limit from the input', async () => {
    const h = await build();
    h.onDailyLimit(inputEvent('12'));
    expect(h.s.dailySpendLimit()).toBe(12);
  });

  it('applies a session limit from the input', async () => {
    const h = await build();
    h.onSessionLimit(inputEvent('3.5'));
    expect(h.s.sessionSpendLimit()).toBe(3.5);
  });

  it('ignores a negative limit rather than storing it', async () => {
    const h = await build();
    const before = h.s.dailySpendLimit();
    h.onDailyLimit(inputEvent('-5'));
    expect(h.s.dailySpendLimit()).toBe(before);
  });

  it('accepts zero, which disables the alert', async () => {
    const h = await build();
    h.onDailyLimit(inputEvent('0'));
    expect(h.s.dailySpendLimit()).toBe(0);
  });

  it('treats a blank input as zero rather than NaN', async () => {
    const h = await build();
    h.onSessionLimit(inputEvent(''));
    expect(h.s.sessionSpendLimit()).toBe(0);
  });

  // ── native notification toggle ─────────────────────────────────────────────

  function checkboxEvent(checked: boolean): Event {
    const el = document.createElement('input');
    el.type = 'checkbox';
    el.checked = checked;
    return { target: el } as unknown as Event;
  }

  it('toggles native notifications from the checkbox', async () => {
    const h = await build();
    h.onNativeNotify(checkboxEvent(false));
    expect(h.s.nativeNotify()).toBe(false);
    h.onNativeNotify(checkboxEvent(true));
    expect(h.s.nativeNotify()).toBe(true);
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders the dashboard without a Tauri runtime', async () => {
    await build();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text.length).toBeGreaterThan(0);
  });

  it('reflects a live daemon frame in the rendered gauge', async () => {
    const h = await build();
    bridge.emit(
      'daemon',
      JSON.stringify({
        type: 'event',
        turn: {
          session_id: 's1',
          model: 'claude-sonnet-4',
          input_tokens: 0,
          output_tokens: 0,
          cache_read_input_tokens: 100_000,
          cache_creation_input_tokens: 0,
        },
      }),
    );
    fixture.detectChanges();
    expect(h.s.fill()).toBe(100_000);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('claude-sonnet-4');
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

  it('ignores a negative session limit rather than storing it', async () => {
    const h = await build();
    const before = h.s.sessionSpendLimit();
    h.onSessionLimit(inputEvent('-1'));
    expect(h.s.sessionSpendLimit()).toBe(before);
  });

  // ── what the controls do when used ─────────────────────────────────────────
  //
  // The handlers above are tested by calling them; these drive the rendered controls,
  // which is the only way to know the template wires each one to the right handler.

  function el<T extends Element>(selector: string): T {
    const found = (fixture.nativeElement as HTMLElement).querySelector<T>(selector);
    expect(found, selector).not.toBeNull();
    return found!;
  }

  function change(selector: string, value: string): void {
    const input = el<HTMLInputElement>(selector);
    input.value = value;
    input.dispatchEvent(new Event('change'));
  }

  it('sets the window tier from the segmented control', async () => {
    const h = await build();
    const tabs = (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.segmented__opt');
    tabs[2].click();
    fixture.detectChanges();
    expect(h.s.contextOverride()).toBe(500_000);
    expect(tabs[2].getAttribute('aria-selected')).toBe('true');
    expect(tabs[0].getAttribute('aria-selected')).toBe('false');
  });

  it('applies the spend limits typed into the inputs', async () => {
    const h = await build();
    change('input[aria-label="Daily spend limit in dollars"]', '7');
    change('input[aria-label="Per-session spend limit in dollars"]', '4');
    expect(h.s.dailySpendLimit()).toBe(7);
    expect(h.s.sessionSpendLimit()).toBe(4);
  });

  it('turns native notifications off from the rendered toggle', async () => {
    const h = await build();
    el<HTMLInputElement>('input[aria-label="Enable native OS notifications"]').click();
    expect(h.s.nativeNotify()).toBe(false);
  });

  // ── what the dashboard shows ───────────────────────────────────────────────

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

  it('badges the Hotspots tab with the recorded fault count', async () => {
    await build((b) => b.responses.set('get_fault_count', 3));
    const badge = el<HTMLElement>('.tab-nav__badge');
    expect(badge.textContent?.trim()).toBe('3');
    expect(badge.getAttribute('aria-label')).toBe('3 faults recorded');
  });

  it('leaves the Hotspots tab unbadged when nothing was recorded', async () => {
    await build((b) => b.responses.set('get_fault_count', 0));
    expect(fixture.nativeElement.querySelector('.tab-nav__badge')).toBeNull();
  });

  it('shows the active alert as a banner at its level', async () => {
    await build();
    bridge.emit('daemon', turn({ cache_read_input_tokens: 195_000 }));
    fixture.detectChanges();
    const banner = el<HTMLElement>('.banner');
    expect(banner.getAttribute('data-level')).toBe('alert');
    expect(banner.textContent).toContain('compaction imminent');
  });

  it('names the project the gauge follows, without a count for one session', async () => {
    const h = await build();
    bridge.emit('daemon', turn({ project: 'lumen' }));
    fixture.detectChanges();
    expect(el<HTMLElement>('.gauge-stage__project').textContent).toContain('lumen');
    expect(fixture.nativeElement.querySelector('.gauge-stage__count')).toBeNull();
    expect(h.projectHint()).toBe('Project: lumen');
  });

  it('says how many other sessions exist when it follows one of several', async () => {
    const h = await build();
    bridge.emit('daemon', turn({ project: 'lumen' }));
    bridge.emit('daemon', turn({ session_id: 's2', project: 'speedash' }));
    fixture.detectChanges();
    expect(el<HTMLElement>('.gauge-stage__count').textContent?.trim()).toBe('+1 more');
    expect(h.projectHint()).toContain('most recently active of 2 sessions: speedash');
  });

  it('renders the usage report once it has loaded', async () => {
    const zero = { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
    await build((b) => b.responses.set('get_usage', {
      rolling5h: zero, windowStart: null, resetApprox: null,
      rolling7dOpus: zero, rolling7dOther: zero, today: zero, thisWeek: zero, allTime: zero,
    }));
    expect(el<HTMLElement>('.usage').textContent).toContain('Usage & Cost');
  });

  it('renders no usage block before the report loads', async () => {
    await build();
    expect(fixture.nativeElement.querySelector('.usage')).toBeNull();
  });
});
