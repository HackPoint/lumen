import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Settings } from './settings';
import { SessionService } from '../../session.service';
import { RepeatReadsService } from '../../repeat-reads.service';
import { BillingModeService } from '../../billing-mode.service';
import { TauriBridge } from '../../tauri-bridge';
import { APP_VERSION_CMD, FakeTauriBridge, PRICE_TABLE } from '../../tauri-bridge.fake';
import type { FaultReport, FilingResult, RepeatReadsSetting } from '../../components/index';
import type { BillingModeSetting } from '../../billing-mode.service';

/**
 * Settings holds what the user can change (billing mode, the alert thresholds, repeat reads),
 * the spend figures the thresholds are set against, fault reporting and the version. Each part
 * came from Home or Hotspots in 1.9.0, except billing mode, which is new. The tests read the
 * rendered page and drive its controls, since the wiring is what moved.
 */
describe('Settings', () => {
  let fixture: ComponentFixture<Settings>;
  let bridge: FakeTauriBridge;

  const SETTINGS_FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: SETTINGS_FILE, ...over };
  }

  function billing(over: Partial<BillingModeSetting> = {}): BillingModeSetting {
    return { mode: null, problem: null, file: SETTINGS_FILE, ...over };
  }

  async function build(seed?: (b: FakeTauriBridge) => void): Promise<Settings> {
    bridge = new FakeTauriBridge();
    // No settings file: repeat reads on by default, and billing not asked yet.
    bridge.responses.set('get_repeat_reads_setting', setting());
    bridge.responses.set('get_billing_mode_setting', billing());
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Settings);
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

  async function open(seed?: (b: FakeTauriBridge) => void): Promise<Settings> {
    const page = await build(seed);
    await settle();
    return page;
  }

  function el<T extends Element>(selector: string): T {
    const found = (fixture.nativeElement as HTMLElement).querySelector<T>(selector);
    expect(found, selector).not.toBeNull();
    return found!;
  }

  function rendered(selector: string): string {
    return (el(selector).textContent ?? '').replace(/\s+/g, ' ').trim();
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
    let resolve!: (v: T) => void;
    const promise = new Promise<T>((r) => {
      resolve = r;
    });
    return { promise, resolve };
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── billing mode ───────────────────────────────────────────────────────────
  //
  // Saved in the settings file beside repeat_reads. Unset is a state of its own, "not chosen
  // yet", and never shown as one of the two.

  describe('billing mode', () => {
    const SECTION = 'section[aria-labelledby="billing-title"]';
    const radio = (mode: string) => el<HTMLInputElement>(`${SECTION} input[type="radio"][value="${mode}"]`);
    const said = () => rendered(SECTION);

    it('offers the two modes, and says it is not chosen yet when the file has no answer', async () => {
      await open();
      const labels = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll(`${SECTION} label`))
        .map((l) => l.textContent?.trim());
      expect(labels).toEqual(['Subscription (Pro/Max)', 'API (pay per token)']);
      expect(radio('subscription').checked).toBe(false);
      expect(radio('api').checked).toBe(false);
      expect(said()).toContain('Not chosen yet.');
    });

    it('shows the mode the file holds', async () => {
      await open((b) => b.responses.set('get_billing_mode_setting', billing({ mode: 'api' })));
      expect(radio('api').checked).toBe(true);
      expect(radio('subscription').checked).toBe(false);
      expect(said()).not.toContain('Not chosen yet');
    });

    it('round-trips a choice: written as chosen, then shown as the file says', async () => {
      await open((b) => b.responses.set('set_billing_mode_setting', billing({ mode: 'subscription' })));
      radio('subscription').click();
      await settle();
      expect(bridge.lastArgsOf('set_billing_mode_setting')).toEqual({ mode: 'subscription' });
      expect(radio('subscription').checked).toBe(true);
      expect(said()).not.toContain('Not chosen yet');
      expect(said()).not.toContain('Not saved');

      bridge.responses.set('set_billing_mode_setting', billing({ mode: 'api' }));
      radio('api').click();
      await settle();
      expect(bridge.lastArgsOf('set_billing_mode_setting')).toEqual({ mode: 'api' });
      expect(radio('api').checked).toBe(true);
      expect(radio('subscription').checked).toBe(false);
    });

    it('puts the choice back, and says why, when the write fails', async () => {
      await open((b) => {
        b.responses.set('get_billing_mode_setting', billing({ mode: 'subscription' }));
        b.failures.add('set_billing_mode_setting');
      });
      radio('api').click();
      await settle();
      expect(radio('api').checked).toBe(false);
      expect(radio('subscription').checked).toBe(true);
      expect(said()).toContain('Not saved: fake: set_billing_mode_setting failed');
    });

    it('says so when the file was written but says something else', async () => {
      await open((b) => b.responses.set('set_billing_mode_setting', billing({ mode: 'subscription' })));
      radio('api').click();
      await settle();
      expect(said()).toContain('The settings file says subscription about billing, not api.');
      expect(radio('api').checked).toBe(false);
    });

    it('holds still while a write is in flight', async () => {
      const write = deferred<BillingModeSetting>();
      let writes = 0;
      await open((b) => {
        b.setBillingModeSetting = () => {
          writes += 1;
          return write.promise;
        };
      });
      radio('api').click();
      await settle();
      expect(radio('api').getAttribute('aria-disabled')).toBe('true');
      radio('subscription').click();
      await settle();
      expect(writes).toBe(1);
      expect(radio('subscription').checked).toBe(false);
      write.resolve(billing({ mode: 'api' }));
      await settle();
      expect(radio('api').checked).toBe(true);
      expect(radio('api').hasAttribute('aria-disabled')).toBe(false);
    });

    it('says what is wrong with a value the file cannot hold, and claims no mode', async () => {
      await open((b) => b.responses.set('get_billing_mode_setting', billing({
        problem: 'billing_mode must be "subscription" or "api", not "pro"',
      })));
      expect(said()).toContain(`The settings file ${SETTINGS_FILE} holds no billing mode Lumen can use`);
      expect(said()).toContain('not "pro"');
      expect(said()).not.toContain('Not chosen yet');
      expect(radio('subscription').checked).toBe(false);
      expect(radio('api').checked).toBe(false);
    });

    it('claims nothing before the file has been read', async () => {
      const read = deferred<BillingModeSetting>();
      await build((b) => {
        b.getBillingModeSetting = () => read.promise;
      });
      expect(said()).toContain('Reading the settings file…');
      expect(fixture.nativeElement.querySelector(`${SECTION} input`)).toBeNull();
    });

    it('says it could not read the mode, offers Retry, and shows what a retry finds', async () => {
      await open((b) => b.failures.add('get_billing_mode_setting'));
      expect(said()).toContain('The billing mode could not be read: fake: get_billing_mode_setting failed.');
      expect(fixture.nativeElement.querySelector(`${SECTION} input`)).toBeNull();
      bridge.failures.delete('get_billing_mode_setting');
      bridge.responses.set('get_billing_mode_setting', billing({ mode: 'api' }));
      el<HTMLButtonElement>(`${SECTION} button`).click();
      await settle();
      expect(radio('api').checked).toBe(true);
    });

    it('shares its state with the first-run question', async () => {
      await open((b) => b.responses.set('set_billing_mode_setting', billing({ mode: 'api' })));
      expect(TestBed.inject(BillingModeService).unasked()).toBe(true);
      radio('api').click();
      await settle();
      expect(TestBed.inject(BillingModeService).unasked()).toBe(false);
    });
  });

  // ── spend: Claude Code's, the figures the thresholds are set against ───────

  describe('spend', () => {
    it('shows the session cost tiles', async () => {
      await open();
      expect(rendered('section.spend cost')).toContain('Session cost');
    });

    it('renders the usage report once it has loaded', async () => {
      const zero = { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
      await open((b) => b.responses.set('get_usage', {
        rolling5h: zero, windowStart: null, resetApprox: null,
        rolling7dOpus: zero, rolling7dOther: zero, today: zero, thisWeek: zero, allTime: zero,
      }));
      expect(rendered('section.spend .usage')).toContain('Usage & Cost');
    });

    it('renders no usage block before the report loads', async () => {
      await open();
      expect(fixture.nativeElement.querySelector('.usage')).toBeNull();
    });

    it('prices the session per model and tags a figure that is not the whole', async () => {
      await open((b) => b.responses.set('lumen_prices', PRICE_TABLE));
      bridge.emit('daemon', JSON.stringify({
        type: 'event',
        turn: {
          session_id: 's1', model: 'claude-sonnet-4-6', speed: 'standard', input_tokens: 0,
          output_tokens: 1_000_000, cache_read_input_tokens: 0, cache_creation_input_tokens: 0,
          cache_creation_1h_input_tokens: 0,
        },
      }));
      fixture.detectChanges();
      expect(rendered('section.spend .tile__value')).toBe('$15.00');
      expect(fixture.nativeElement.querySelector('section.spend .partial-tag')).toBeNull();

      bridge.emit('daemon', JSON.stringify({
        type: 'event',
        turn: {
          session_id: 's1', model: '<synthetic>', input_tokens: 0, output_tokens: 10,
          cache_read_input_tokens: 0, cache_creation_input_tokens: 0,
        },
      }));
      fixture.detectChanges();
      expect(rendered('section.spend .partial-tag')).toBe('at least');
    });

    it('prices the usage report from the price table', async () => {
      const zero = { turns: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
      const today = {
        ...zero, turns: 1, output: 1_000_000, totalTokens: 1_000_000,
        byModel: [{
          model: 'claude-haiku-4-5-20251001', speed: 'standard', turns: 1, input: 0,
          output: 1_000_000, cacheRead: 0, cacheWrite: 0, cacheWrite1h: 0, cacheWriteUnknown: 0,
        }],
      };
      await open((b) => {
        b.responses.set('lumen_prices', PRICE_TABLE);
        b.responses.set('get_usage', {
          rolling5h: zero, windowStart: null, resetApprox: null,
          rolling7dOpus: zero, rolling7dOther: zero, today, thisWeek: zero, allTime: zero,
        });
      });
      expect(rendered('section.spend .usage__rollups')).toContain('$5.00');
    });
  });

  // ── alert thresholds ───────────────────────────────────────────────────────

  describe('alert thresholds', () => {
    function inputEvent(value: string): Event {
      const input = document.createElement('input');
      input.value = value;
      return { target: input } as unknown as Event;
    }

    function change(selector: string, value: string): void {
      const input = el<HTMLInputElement>(selector);
      input.value = value;
      input.dispatchEvent(new Event('change'));
    }

    it('shows the thresholds as they stand', async () => {
      await open();
      expect(el<HTMLInputElement>('input[aria-label="Daily spend limit in dollars"]').value).toBe('5');
      expect(el<HTMLInputElement>('input[aria-label="Per-session spend limit in dollars"]').value).toBe('2');
      expect(rendered('section.limits')).toContain('0 disables a spend alert. Resets to defaults on restart.');
    });

    it('applies the spend limits typed into the inputs', async () => {
      const page = await open();
      change('input[aria-label="Daily spend limit in dollars"]', '7');
      change('input[aria-label="Per-session spend limit in dollars"]', '4');
      expect(page.s.dailySpendLimit()).toBe(7);
      expect(page.s.sessionSpendLimit()).toBe(4);
    });

    it('round-trips a threshold: what was set is what the input then shows', async () => {
      const page = await open();
      change('input[aria-label="Daily spend limit in dollars"]', '12');
      fixture.detectChanges();
      expect(page.s.dailySpendLimit()).toBe(12);
      expect(el<HTMLInputElement>('input[aria-label="Daily spend limit in dollars"]').value).toBe('12');
    });

    it('ignores a negative limit rather than storing it', async () => {
      const page = await open();
      page.onDailyLimit(inputEvent('-5'));
      page.onSessionLimit(inputEvent('-1'));
      expect(page.s.dailySpendLimit()).toBe(5);
      expect(page.s.sessionSpendLimit()).toBe(2);
    });

    it('accepts zero, which disables the alert, and reads a blank input as zero rather than NaN', async () => {
      const page = await open();
      page.onDailyLimit(inputEvent('0'));
      page.onSessionLimit(inputEvent(''));
      expect(page.s.dailySpendLimit()).toBe(0);
      expect(page.s.sessionSpendLimit()).toBe(0);
    });

    it('says under the thresholds when a limit cannot be checked against unpriced spend', async () => {
      const page = await open((b) => b.responses.set('lumen_prices', PRICE_TABLE));
      bridge.emit('daemon', JSON.stringify({
        type: 'event',
        turn: {
          session_id: 's1', model: '<synthetic>', input_tokens: 0, output_tokens: 1_000_000,
          cache_read_input_tokens: 0, cache_creation_input_tokens: 0,
        },
      }));
      fixture.detectChanges();
      const notes = [...(fixture.nativeElement as HTMLElement)
        .querySelectorAll('section.limits .limits__hint[role="status"]')]
        .map((n) => n.textContent?.trim());
      expect(notes).toEqual([
        'The per-session limit of $2 cannot be checked: none of that spend has a price. '
          + 'No price for <synthetic>.',
      ]);
      expect(page.s.notification()).toBeNull();
    });

    it('turns native notifications off and on from the rendered toggle', async () => {
      const page = await open();
      const box = el<HTMLInputElement>('input[aria-label="Enable native OS notifications"]');
      box.click();
      expect(page.s.nativeNotify()).toBe(false);
      box.click();
      expect(page.s.nativeNotify()).toBe(true);
    });
  });

  // ── repeat reads switch ────────────────────────────────────────────────────
  //
  // On Home until 1.9.0. The switch shows the settings file and the default, never the server's
  // effective mode:
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
      return rendered(SECTION);
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

  // ── About ──────────────────────────────────────────────────────────────────
  //
  // On Home until 1.9.0. Read from the Tauri runtime, never written in the page: these use a
  // version no release has, so a literal in the template or the component would fail them.
  // The built app's text is checked by lumenator/e2e.

  describe('About', () => {
    const SEEDED = '9.8.7';

    it('shows the version the runtime returned', async () => {
      await open((b) => b.responses.set(APP_VERSION_CMD, SEEDED));
      expect(el<HTMLElement>('.about__version').textContent).toBe(SEEDED);
    });

    it('follows whatever version the runtime returns', async () => {
      await open((b) => b.responses.set(APP_VERSION_CMD, '0.0.1-rc.2'));
      expect(el<HTMLElement>('.about__version').textContent).toBe('0.0.1-rc.2');
    });

    it('labels the section by its heading, and the version by its term', async () => {
      await open((b) => b.responses.set(APP_VERSION_CMD, SEEDED));
      const section = el<HTMLElement>('section.about');
      const heading = fixture.nativeElement.querySelector(`#${section.getAttribute('aria-labelledby')}`);
      expect(heading?.textContent?.trim()).toBe('About');
      const list = el<HTMLElement>('.about__list');
      expect(list.querySelector('dt')?.textContent?.trim()).toBe('Version');
      expect(list.querySelector('dt + dd')?.textContent).toBe(SEEDED);
    });

    it('shows nothing until the runtime has given a version', async () => {
      await build((b) => {
        vi.spyOn(b, 'getVersion').mockReturnValue(new Promise<string>(() => {}));
      });
      expect(el<HTMLElement>('.about__version').textContent).toBe('');
    });

    it('says the version is unavailable when the runtime cannot give it', async () => {
      const page = await open((b) => b.failures.add(APP_VERSION_CMD));
      expect(page.version()).toBeNull();
      expect(el<HTMLElement>('.about__version').textContent).toBe('unavailable');
    });
  });

  // ── Report a fault (on Hotspots until 1.9.0) ───────────────────────────────

  describe('fault report', () => {
    // ── Fault report ────────────────────────────────────────────────────────────
    //
    // Filing publishes to a public tracker and cannot be undone, so these tests are
    // mostly about what must NOT happen: no send on mount, no send on preview, no
    // second send, and never a body the user was not shown.

    function faultReport(over: Partial<FaultReport> = {}): FaultReport {
      return {
        body: '### lumen 1.4.0 — retry escape valve fired 7x on 2 files\n\nbody text',
        title: 'lumen 1.4.0 — retry escape valve fired 7x on 2 files',
        fingerprint: 'ffd15312',
        kinds: 2,
        occurrences: 7,
        repo: 'HackPoint/lumen',
        ...over,
      };
    }

    /**
     * Flush the invoke promise chain, not just its first link.
     *
     * `then(set url)` and `finally(clear filing)` are separate microtasks, so two awaits
     * land between them: the URL is on screen while the button still says "Filing…". Six
     * drains the whole chain, and awaiting `whenStable()` is not an option — SessionService
     * holds a live daemon subscription, so the fixture is never stable.
     */
    function filingResult(over: Partial<FilingResult> = {}): FilingResult {
      return {
        url: 'https://github.com/HackPoint/lumen/issues/1',
        route: 'gh',
        handoff: false,
        commented: false,
        fellBack: [],
        ...over,
      };
    }

    async function tick(): Promise<void> {
      for (let i = 0; i < 6; i++) await Promise.resolve();
      fixture.detectChanges();
    }

    function button(label: string): HTMLButtonElement | undefined {
      const all = Array.from(
        (fixture.nativeElement as HTMLElement).querySelectorAll('button'),
      ) as HTMLButtonElement[];
      return all.find((b) => (b.textContent ?? '').trim().startsWith(label));
    }

    it('does not touch the fault backend until asked', async () => {
      await open();
      expect(bridge.countOf('get_fault_report')).toBe(0);
      expect(bridge.countOf('file_fault_report')).toBe(0);
      expect(button('File issue')).toBeUndefined();
    });

    it('checking for faults renders locally and files nothing', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());

      button('Check for faults')!.click();
      await tick();

      expect(bridge.countOf('get_fault_report')).toBe(1);
      expect(bridge.countOf('file_fault_report')).toBe(0);
      expect(text()).toContain('this is the exact text that will be filed');
      expect(text()).toContain('2 fault groups, 7 occurrences');
    });

    it('files only on the second, explicit click, and sends the shown body', async () => {
      await open();
      const r = faultReport();
      bridge.responses.set('get_fault_report', r);
      bridge.responses.set('file_fault_report', filingResult());

      button('Check for faults')!.click();
      await tick();
      button('File issue')!.click();
      await tick();

      expect(bridge.countOf('file_fault_report')).toBe(1);
      // Byte-for-byte the text on screen: re-rendering could file something else.
      expect(bridge.lastArgsOf('file_fault_report')).toEqual({
        body: r.body,
        title: r.title,
        fingerprint: r.fingerprint,
        repo: r.repo,
      });
      expect(text()).toContain('https://github.com/HackPoint/lumen/issues/1');
    });

    it('will not file the same report twice', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());
      bridge.responses.set('file_fault_report', filingResult({ url: 'https://example.test/1' }));

      button('Check for faults')!.click();
      await tick();
      button('File issue')!.click();
      await tick();

      const filed = button('Filed');
      expect(filed).toBeDefined();
      expect(filed!.disabled).toBe(true);

      filed!.click();
      await tick();
      expect(bridge.countOf('file_fault_report')).toBe(1);
    });

    it('says so when there is nothing to report, and offers no file button', async () => {
      await open();
      bridge.responses.set('get_fault_report', null);

      button('Check for faults')!.click();
      await tick();

      expect(text()).toContain('No faults recorded');
      expect(button('File issue')).toBeUndefined();
    });

    it('says why a report could not be rendered, and offers nothing to file', async () => {
      await open();
      bridge.failures.add('get_fault_report');

      button('Check for faults')!.click();
      await tick();

      const err = (fixture.nativeElement as HTMLElement).querySelector('.fr__error');
      expect(err?.getAttribute('role')).toBe('alert');
      expect(err?.textContent).toContain('fake: get_fault_report failed');
      expect(button('File issue')).toBeUndefined();
      expect(text()).not.toContain('No faults recorded');
    });

    it('surfaces a filing failure instead of claiming success', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());
      bridge.failures.add('file_fault_report');

      button('Check for faults')!.click();
      await tick();
      button('File issue')!.click();
      await tick();

      expect(text()).toContain('fake: file_fault_report failed');
      expect(text()).not.toContain('Filed:');
    });

    it('dismissing drops the report without filing it', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());

      button('Check for faults')!.click();
      await tick();
      button('Dismiss')!.click();
      await tick();

      expect(bridge.countOf('file_fault_report')).toBe(0);
      expect(text()).not.toContain('this is the exact text that will be filed');
    });
    it('badges the Settings tab, where this is, only when faults are waiting', async () => {
      await open();
      expect(
        (fixture.nativeElement as HTMLElement).querySelector('.tab-nav__badge'),
      ).toBeNull();

      bridge.responses.set('get_fault_count', 4);
      fixture.componentInstance.s.refreshFaultCount();
      await tick();

      const badge = (fixture.nativeElement as HTMLElement).querySelector('a[href="/settings"] .tab-nav__badge');
      expect(badge).not.toBeNull();
      expect(badge!.textContent?.trim()).toBe('4');
      expect(badge!.getAttribute('aria-label')).toBe('4 faults recorded');
      expect(rendered('.fr__count')).toBe('4 recorded');
    });

    it('sits just above About, which names the build a report comes from', async () => {
      await open();
      const fr = el<HTMLElement>('section.fr');
      expect(fr.nextElementSibling?.matches('section.about')).toBe(true);
    });

    it('does not call a browser handoff "filed" — nothing is posted yet', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());
      bridge.responses.set('file_fault_report', filingResult({
        url: 'https://github.com/HackPoint/lumen/issues/new?title=x',
        route: 'browser',
        handoff: true,
      }));

      button('Check for faults')!.click();
      await tick();
      button('File issue')!.click();
      await tick();

      const t = text();
      expect(t).toContain('submit it there');
      expect(t).toContain('Nothing has been posted yet');
      expect(t).not.toContain('Filed via');
      // The button must not claim it either.
      expect(button('Filed')).toBeUndefined();
      expect(button('Opened')).toBeDefined();
    });

    it('names the route and shows any fallbacks it went through', async () => {
      await open();
      bridge.responses.set('get_fault_report', faultReport());
      bridge.responses.set('file_fault_report', filingResult({
        route: 'api',
        fellBack: ['gh: cannot run gh (is the GitHub CLI installed?)'],
      }));

      button('Check for faults')!.click();
      await tick();
      button('File issue')!.click();
      await tick();

      const t = text();
      expect(t).toContain('Filed');
      expect(t).toContain('api');
      // A silent fallback would hide that the preferred route is broken.
      expect(t).toContain('cannot run gh');
    });
  });
});
