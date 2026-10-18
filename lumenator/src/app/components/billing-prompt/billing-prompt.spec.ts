import { ComponentFixture, TestBed } from '@angular/core/testing';
import { BillingPrompt } from './billing-prompt';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { BillingModeSetting } from '../../billing-mode.service';

/**
 * The first-run billing question. Home decides where and when it is hosted (pinned in Home's
 * spec); these pin when it shows, what a choice does, and that it is gone afterwards.
 */
describe('BillingPrompt', () => {
  let fixture: ComponentFixture<BillingPrompt>;
  let bridge: FakeTauriBridge;
  const FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<BillingModeSetting> = {}): BillingModeSetting {
    return { mode: null, problem: null, file: FILE, ...over };
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 8; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  async function mount(seed?: (b: FakeTauriBridge) => void): Promise<void> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_billing_mode_setting', setting());
    seed?.(bridge);
    TestBed.configureTestingModule({ providers: [{ provide: TauriBridge, useValue: bridge }] });
    fixture = TestBed.createComponent(BillingPrompt);
    fixture.detectChanges();
    await settle();
  }

  function prompt(): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('section.prompt');
  }

  function choice(label: string): HTMLButtonElement {
    const all = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('button'));
    const found = all.find((b) => b.textContent?.trim() === label);
    expect(found, label).toBeDefined();
    return found!;
  }

  afterEach(() => TestBed.resetTestingModule());

  it('asks once the file is read and holds no answer, with the two modes', async () => {
    await mount();
    expect(bridge.countOf('get_billing_mode_setting')).toBe(1);
    expect(prompt()?.textContent).toContain('How do you pay for Claude Code?');
    expect(prompt()?.textContent).toContain('You can change it later in Settings.');
    expect(choice('Subscription (Pro/Max)')).toBeTruthy();
    expect(choice('API (pay per token)')).toBeTruthy();
  });

  it('writes the choice and is gone after it', async () => {
    await mount((b) => b.responses.set('set_billing_mode_setting', setting({ mode: 'subscription' })));
    choice('Subscription (Pro/Max)').click();
    await settle();
    expect(bridge.lastArgsOf('set_billing_mode_setting')).toEqual({ mode: 'subscription' });
    expect(prompt()).toBeNull();
  });

  it('stays, and says why, when the choice could not be saved, and goes once a retry takes', async () => {
    await mount((b) => b.failures.add('set_billing_mode_setting'));
    choice('API (pay per token)').click();
    await settle();
    expect(prompt()?.querySelector('[role="alert"]')?.textContent)
      .toBe('Not saved: fake: set_billing_mode_setting failed');
    bridge.failures.delete('set_billing_mode_setting');
    bridge.responses.set('set_billing_mode_setting', setting({ mode: 'api' }));
    choice('API (pay per token)').click();
    await settle();
    expect(prompt()).toBeNull();
  });

  it('stays to say why when the file was written but cannot be read back', async () => {
    await mount((b) => b.responses.set('set_billing_mode_setting', setting({ problem: 'not JSON: x' })));
    choice('API (pay per token)').click();
    await settle();
    expect(prompt()?.querySelector('[role="alert"]')?.textContent)
      .toBe('Not saved: The settings file says nothing about billing, not api. not JSON: x.');
  });

  it('does not ask when the file already holds an answer', async () => {
    await mount((b) => b.responses.set('get_billing_mode_setting', setting({ mode: 'api' })));
    expect(prompt()).toBeNull();
  });

  it('does not ask over a file it cannot use, which Settings explains', async () => {
    await mount((b) => b.responses.set('get_billing_mode_setting', setting({ problem: 'not JSON: x' })));
    expect(prompt()).toBeNull();
  });

  it('does not ask when the setting cannot be read at all', async () => {
    await mount((b) => b.failures.add('get_billing_mode_setting'));
    expect(prompt()).toBeNull();
  });

  it('does not ask before the file has been read', async () => {
    bridge = new FakeTauriBridge();
    bridge.getBillingModeSetting = () => new Promise<BillingModeSetting>(() => {});
    TestBed.configureTestingModule({ providers: [{ provide: TauriBridge, useValue: bridge }] });
    fixture = TestBed.createComponent(BillingPrompt);
    fixture.detectChanges();
    await settle();
    expect(prompt()).toBeNull();
  });

  it('sends one choice at a time', async () => {
    let writes = 0;
    let finish!: (v: BillingModeSetting) => void;
    await mount((b) => {
      b.setBillingModeSetting = () => {
        writes += 1;
        return new Promise<BillingModeSetting>((r) => (finish = r));
      };
    });
    choice('API (pay per token)').click();
    await settle();
    expect(choice('Subscription (Pro/Max)').disabled).toBe(true);
    await fixture.componentInstance.choose('subscription');
    expect(writes).toBe(1);
    finish(setting({ mode: 'api' }));
    await settle();
    expect(prompt()).toBeNull();
  });

  // ── what a screen reader and the keyboard get after a choice ────────────────
  //
  // The button that was chosen goes with the prompt. Focus has to land somewhere on purpose, or
  // it drops to the page body, and the choice has to be said, or nothing confirms it.

  describe('after a choice', () => {
    const STATUS = '[role="status"]';

    function status(): HTMLElement | null {
      return (fixture.nativeElement as HTMLElement).querySelector(STATUS);
    }

    it('has a polite live region in place, and empty, before anything is chosen', async () => {
      await mount();
      const region = status();
      expect(region).not.toBeNull();
      expect(region!.getAttribute('aria-live')).toBe('polite');
      expect(region!.textContent?.trim()).toBe('');
    });

    it('says what was saved, and moves focus to that sentence rather than dropping it', async () => {
      await mount((b) => b.responses.set('set_billing_mode_setting', setting({ mode: 'api' })));
      const region = status();
      const button = choice('API (pay per token)');
      button.focus();
      button.click();
      await settle();
      expect(prompt()).toBeNull();
      // The same element that was there before: a region that appears with its text in it is
      // not reliably announced.
      expect(status()).toBe(region);
      expect(status()!.textContent?.trim())
        .toBe('Billing mode saved: API (pay per token). You can change it in Settings.');
      expect(document.activeElement).toBe(status());
      expect(document.activeElement).not.toBe(document.body);
    });

    it('names the subscription choice the same way', async () => {
      await mount((b) => b.responses.set('set_billing_mode_setting', setting({ mode: 'subscription' })));
      choice('Subscription (Pro/Max)').click();
      await settle();
      expect(status()!.textContent?.trim())
        .toBe('Billing mode saved: Subscription (Pro/Max). You can change it in Settings.');
    });

    it('says nothing was saved, and leaves focus on the button, when the write fails', async () => {
      await mount((b) => b.failures.add('set_billing_mode_setting'));
      const button = choice('API (pay per token)');
      button.focus();
      button.click();
      await settle();
      expect(status()!.textContent?.trim()).toBe('');
      expect(document.activeElement).toBe(choice('API (pay per token)'));
    });
  });
});
