import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Home } from './home';
import { SessionService } from '../../session.service';
import { RepeatReadsService } from '../../repeat-reads.service';
import { TauriBridge } from '../../tauri-bridge';
import { APP_VERSION_CMD, FakeTauriBridge } from '../../tauri-bridge.fake';
import type { RepeatReadsSetting } from '../../components/index';

describe('Home', () => {
  let fixture: ComponentFixture<Home>;
  let bridge: FakeTauriBridge;

  const SETTINGS_FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: SETTINGS_FILE, ...over };
  }

  async function build(seed?: (b: FakeTauriBridge) => void): Promise<Home> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_setup_needed', false);
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

  // ── the version ────────────────────────────────────────────────────────────
  //
  // The version was only ever in a startup log line, so a user filing a report could not say
  // which build they had. It is read from the Tauri runtime, never written in the page: these
  // use a version no release has, so a literal in the template or the component would fail them.
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

  it('shows the same version in the About section', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, SEEDED));
    expect(el<HTMLElement>('.about__version').textContent).toBe(SEEDED);
  });

  it('follows whatever version the runtime returns', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, '0.0.1-rc.2'));
    expect(el<HTMLElement>('.home__version').textContent).toBe('Lumen 0.0.1-rc.2');
    expect(el<HTMLElement>('.about__version').textContent).toBe('0.0.1-rc.2');
  });

  it('asks the runtime for the version once', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, SEEDED));
    expect(bridge.countOf(APP_VERSION_CMD)).toBe(1);
  });

  it('labels the About section by its heading, and the version by its term', async () => {
    await build(b => b.responses.set(APP_VERSION_CMD, SEEDED));
    const section = el<HTMLElement>('section.about');
    const heading = fixture.nativeElement.querySelector(`#${section.getAttribute('aria-labelledby')}`);
    expect(heading?.textContent?.trim()).toBe('About');
    const list = el<HTMLElement>('.about__list');
    expect(list.querySelector('dt')?.textContent?.trim()).toBe('Version');
    expect(list.querySelector('dt + dd')?.textContent).toBe(SEEDED);
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
    expect(el<HTMLElement>('.about__version').textContent).toBe('');
  });

  it('says the version is unavailable when the runtime cannot give it', async () => {
    const h = await build(b => b.failures.add(APP_VERSION_CMD));
    expect(h.version()).toBeNull();
    expect(fixture.nativeElement.querySelector('.home__version')).toBeNull();
    expect(el<HTMLElement>('.about__version').textContent).toBe('unavailable');
  });

  // ── repeat reads switch ────────────────────────────────────────────────────
  //
  // The switch shows the settings file and the default, never the server's effective mode:
  // a LUMEN_REPEAT_READS value in a Claude Code MCP config outranks the file, and the app
  // cannot see it. Each state is asserted on the words shown and on the checkbox itself.

  describe('repeat reads switch', () => {
    const SECTION = 'section[aria-label="Repeat reads"]';
    const BOX = `${SECTION} input[type="checkbox"]`;
    const FILE_OFF = setting({ on: false, source: 'file' });
    const FILE_ON = setting({ on: true, source: 'file' });
    const CORRUPT = setting({
      on: false,
      source: 'file',
      problem: 'not JSON: expected value at line 1 column 1',
    });

    /** What the section says, whitespace collapsed. */
    function said(): string {
      return (el<HTMLElement>(SECTION).textContent ?? '').replace(/\s+/g, ' ').trim();
    }

    async function settle(): Promise<void> {
      for (let i = 0; i < 10; i++) await Promise.resolve();
      fixture.detectChanges();
    }

    function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
      let resolve!: (v: T) => void;
      const promise = new Promise<T>((r) => {
        resolve = r;
      });
      return { promise, resolve };
    }

    async function open(seed?: (b: FakeTauriBridge) => void): Promise<void> {
      await build(seed);
      await settle();
    }

    it('reads the settings file when the window opens', async () => {
      await open();
      expect(bridge.countOf('get_repeat_reads_setting')).toBe(1);
    });

    it('shows on, and says it is the default, when there is no settings file', async () => {
      await open();
      expect(said()).toContain('On by default. The settings file does not turn it off.');
      expect(el<HTMLInputElement>(BOX).checked).toBe(true);
    });

    it('shows off, as the settings file says, when the file turns it off', async () => {
      await open((b) => b.responses.set('get_repeat_reads_setting', FILE_OFF));
      expect(said()).toContain('Off, as the settings file says. Every read returns its full result.');
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
    });

    it('shows on, as the settings file says, when the file turns it on', async () => {
      await open((b) => b.responses.set('get_repeat_reads_setting', FILE_ON));
      expect(said()).toContain('On, as the settings file says.');
      expect(said()).not.toContain('by default');
      expect(el<HTMLInputElement>(BOX).checked).toBe(true);
    });

    it('says a settings file that could not be read is why repeat reads are off, and what fixes it', async () => {
      await open((b) => b.responses.set('get_repeat_reads_setting', CORRUPT));
      const words = said();
      expect(words).toContain(`The settings file ${SETTINGS_FILE} could not be read`);
      expect(words).toContain('not JSON: expected value at line 1 column 1');
      expect(words).toContain('Repeat reads are off until it is fixed.');
      expect(words).toContain('Switching on again rewrites the file.');
      expect(words).not.toContain('On by default');
      expect(words).not.toContain('as the settings file says');
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
    });

    it('says when a change applies, that it is saved, and that an environment value outranks it', async () => {
      await open();
      const words = said();
      expect(words).toContain('applies to the next Lumen read in every running Claude Code session, with no restart');
      expect(words).toContain(`saved in ${SETTINGS_FILE}, so unlike the alert thresholds above it survives a restart`);
      expect(words).toContain('LUMEN_REPEAT_READS value in a Claude Code MCP config takes priority over the file');
      expect(words).toContain('LUMEN_DB there that points at another ledger');
      expect(words).toContain('This window cannot see either.');
      expect(words).toContain('is not told about the fresh option until Claude Code restarts');
    });

    it('says the same about the environment when the file turns it off', async () => {
      await open((b) => b.responses.set('get_repeat_reads_setting', FILE_OFF));
      expect(said()).toContain('LUMEN_REPEAT_READS value in a Claude Code MCP config takes priority over the file');
    });

    it('writes the choice and shows what the file then says', async () => {
      await open((b) => b.responses.set('set_repeat_reads_setting', FILE_OFF));
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(bridge.lastArgsOf('set_repeat_reads_setting')).toEqual({ on: false });
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
      expect(said()).toContain('Off, as the settings file says.');
      expect(said()).not.toContain('Not saved');
    });

    it('rewrites a settings file that could not be read when it is switched on', async () => {
      await open((b) => {
        b.responses.set('get_repeat_reads_setting', CORRUPT);
        b.responses.set('set_repeat_reads_setting', FILE_ON);
      });
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(bridge.lastArgsOf('set_repeat_reads_setting')).toEqual({ on: true });
      expect(said()).toContain('On, as the settings file says.');
      expect(said()).not.toContain('could not be read');
      expect(el<HTMLInputElement>(BOX).checked).toBe(true);
    });

    it('puts the switch back, and shows why as text, when the write fails', async () => {
      await open((b) => b.failures.add('set_repeat_reads_setting'));
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(el<HTMLInputElement>(BOX).checked).toBe(true);
      expect(el<HTMLElement>(`${SECTION} [role="alert"]`).textContent).toContain(
        'Not saved: fake: set_repeat_reads_setting failed',
      );
      expect(said()).toContain('On by default');
    });

    it('puts the switch back to off when switching on fails', async () => {
      await open((b) => {
        b.responses.set('get_repeat_reads_setting', FILE_OFF);
        b.failures.add('set_repeat_reads_setting');
      });
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
      expect(said()).toContain('Not saved: fake: set_repeat_reads_setting failed');
      expect(said()).toContain('Off, as the settings file says.');
    });

    it('shows the file as it is when the write did not leave it as asked', async () => {
      await open((b) => b.responses.set('set_repeat_reads_setting', FILE_ON));
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(el<HTMLInputElement>(BOX).checked).toBe(true);
      expect(said()).toContain('The settings file says repeat reads are on, not off.');
    });

    it('forgets the error once a later write takes', async () => {
      await open((b) => b.failures.add('set_repeat_reads_setting'));
      el<HTMLInputElement>(BOX).click();
      await settle();
      bridge.failures.delete('set_repeat_reads_setting');
      bridge.responses.set('set_repeat_reads_setting', FILE_OFF);
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(said()).not.toContain('Not saved');
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
    });

    it('holds the switch still while the write is in flight, and keeps the keyboard focus on it', async () => {
      const write = deferred<RepeatReadsSetting>();
      let writes = 0;
      await open((b) => {
        b.setRepeatReadsSetting = () => {
          writes += 1;
          return write.promise;
        };
      });
      const box = el<HTMLInputElement>(BOX);
      box.focus();
      box.click();
      await settle();
      expect(box.getAttribute('aria-disabled')).toBe('true');
      expect(box.disabled).toBe(false);
      expect(document.activeElement).toBe(box);
      box.click();
      await settle();
      expect(writes).toBe(1);
      expect(box.checked).toBe(false);
      write.resolve(FILE_OFF);
      await settle();
      expect(box.hasAttribute('aria-disabled')).toBe(false);
      expect(document.activeElement).toBe(box);
      box.click();
      await settle();
      expect(writes).toBe(2);
    });

    it('claims nothing about on or off before the file has been read', async () => {
      const read = deferred<RepeatReadsSetting>();
      await build((b) => {
        b.getRepeatReadsSetting = () => read.promise;
      });
      expect(said()).toContain('Reading the settings file…');
      expect(fixture.nativeElement.querySelector(BOX)).toBeNull();
      expect(said()).not.toMatch(/\bOn\b|\bOff\b/);
    });

    it('says it could not read the setting, and does not pretend on or off, when the read fails', async () => {
      await open((b) => b.failures.add('get_repeat_reads_setting'));
      expect(said()).toContain('fake: get_repeat_reads_setting failed');
      expect(said()).toContain('Lumen does not know whether repeat reads are on or off.');
      expect(fixture.nativeElement.querySelector(BOX)).toBeNull();
      expect(said()).not.toMatch(/\bOn\b|\bOff\b/);
    });

    it('reads again from Retry, and shows what it finds', async () => {
      await open((b) => b.failures.add('get_repeat_reads_setting'));
      bridge.failures.delete('get_repeat_reads_setting');
      bridge.responses.set('get_repeat_reads_setting', FILE_OFF);
      el<HTMLButtonElement>(`${SECTION} button`).click();
      await settle();
      expect(bridge.countOf('get_repeat_reads_setting')).toBe(2);
      expect(said()).toContain('Off, as the settings file says.');
      expect(el<HTMLInputElement>(BOX).checked).toBe(false);
    });

    it('shows both failures when the file could not be read again before a write failed', async () => {
      await open();
      bridge.failures.add('get_repeat_reads_setting');
      bridge.failures.add('set_repeat_reads_setting');
      const box = el<HTMLInputElement>(BOX);
      void TestBed.inject(RepeatReadsService).load();
      box.click();
      await settle();
      expect(fixture.nativeElement.querySelector(BOX)).toBeNull();
      expect(said()).toContain('fake: get_repeat_reads_setting failed');
      expect(said()).toContain('Lumen does not know whether repeat reads are on or off.');
      expect(said()).toContain('Not saved: fake: set_repeat_reads_setting failed');
    });

    it('does not show a write error left from before the window was opened again', async () => {
      await open((b) => b.failures.add('set_repeat_reads_setting'));
      el<HTMLInputElement>(BOX).click();
      await settle();
      expect(said()).toContain('Not saved');
      await TestBed.inject(RepeatReadsService).load();
      await settle();
      expect(said()).not.toContain('Not saved');
    });
  });
});
