import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { IdentityHookPrompt } from './identity-hook-prompt';
import { RepeatReadsService } from '../../repeat-reads.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';
import type { IdentityHookStatus, RepeatReadsSetting } from '../index';

/**
 * 1.8.0 turns repeat reads on by default, and they work only once one hook is registered in
 * Claude Code's settings, which Setup does. Setup runs by itself on a first launch and never
 * again, so anyone who set Lumen up under 1.7.0 or earlier has repeat reads switched on and
 * every read answered in full, and nothing in the app says so. This prompt says so. It has to
 * be there exactly when the feature is promised and cannot work, and gone the moment it can.
 */
describe('IdentityHookPrompt', () => {
  let fixture: ComponentFixture<IdentityHookPrompt>;
  let bridge: FakeTauriBridge;

  const SETTINGS_FILE = '/Users/me/.lumen/lumen_settings.json';
  const MISSING: IdentityHookStatus = { registered: false, servedElsewhere: false };
  const REGISTERED: IdentityHookStatus = { registered: true, servedElsewhere: false };
  const ELSEWHERE: IdentityHookStatus = { registered: false, servedElsewhere: true };

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: SETTINGS_FILE, ...over };
  }

  /**
   * The prompt as a page hosts it. Home and the Optimizer read the repeat-reads setting when
   * they open, and the prompt takes it from the service they share, so `reads` stands in for
   * them. The defaults are an upgrader: Setup done, the hook missing, repeat reads on.
   */
  async function build(
    seed?: (b: FakeTauriBridge) => void,
    reads = true,
  ): Promise<IdentityHookPrompt> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_setup_needed', false);
    bridge.responses.set('lumen_identity_hook_status', MISSING);
    bridge.responses.set('get_repeat_reads_setting', setting());
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }],
    });
    if (reads) void TestBed.inject(RepeatReadsService).load();
    fixture = TestBed.createComponent(IdentityHookPrompt);
    fixture.detectChanges();
    await settle();
    return fixture.componentInstance;
  }

  /** Let the questions be answered, then render. */
  async function settle(): Promise<void> {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  function banner(): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('[role="status"]');
  }

  /** What the prompt says, whitespace collapsed. */
  function said(): string {
    return ((fixture.nativeElement as HTMLElement).textContent ?? '').replace(/\s+/g, ' ').trim();
  }

  function button(): HTMLButtonElement {
    return (fixture.nativeElement as HTMLElement).querySelector('button') as HTMLButtonElement;
  }

  function focusWindow(): void {
    window.dispatchEvent(new Event('focus'));
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── when it shows ──────────────────────────────────────────────────────────

  it('says repeat reads are on but not working yet, and why, when the hook is not registered', async () => {
    await build();
    expect(banner()).not.toBeNull();
    expect(said()).toContain('Repeat reads are on, but not working yet');
    expect(said()).toContain('cannot tell which conversation a call belongs to');
    expect(said()).toContain('until Setup registers one hook');
  });

  it('is announced as a status, like the banners beside it', async () => {
    await build();
    const b = banner()!;
    expect(b.getAttribute('role')).toBe('status');
    expect(b.getAttribute('aria-live')).toBe('polite');
  });

  it('has a Run Setup button that a keyboard can use', async () => {
    await build();
    // A native button takes Tab, and Enter and Space press it. A link or a div would not.
    expect(button().type).toBe('button');
    expect(button().disabled).toBe(false);
    expect(button().getAttribute('tabindex')).toBeNull();
    expect(button().textContent?.trim()).toBe('Run Setup');
  });

  it('asks each question once when it opens, with no arguments', async () => {
    await build();
    expect(bridge.countOf('lumen_setup_needed')).toBe(1);
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(1);
    expect(bridge.lastArgsOf('lumen_identity_hook_status')).toBeUndefined();
  });

  it('takes the repeat-reads setting from the page and never reads the file itself', async () => {
    await build();
    expect(bridge.countOf('get_repeat_reads_setting')).toBe(1);
  });

  // ── when it stays out of the way ───────────────────────────────────────────

  it('shows nothing once the hook is registered', async () => {
    await build((b) => b.responses.set('lumen_identity_hook_status', REGISTERED));
    expect(banner()).toBeNull();
    expect(said()).toBe('');
  });

  it('shows nothing when the calls are identified by a plugin install, as the ledger shows', async () => {
    await build((b) => b.responses.set('lumen_identity_hook_status', ELSEWHERE));
    expect(banner()).toBeNull();
  });

  it('shows nothing while Setup is still to run, since the Setup page says that', async () => {
    await build((b) => b.responses.set('lumen_setup_needed', true));
    expect(banner()).toBeNull();
  });

  it('shows nothing when repeat reads are off, as there is nothing to wait for', async () => {
    await build((b) =>
      b.responses.set('get_repeat_reads_setting', setting({ on: false, source: 'file' })),
    );
    expect(banner()).toBeNull();
  });

  it('shows nothing when the settings file could not be used, which leaves repeat reads off', async () => {
    await build((b) =>
      b.responses.set(
        'get_repeat_reads_setting',
        setting({ on: false, source: 'file', problem: 'not JSON: expected value' }),
      ),
    );
    expect(banner()).toBeNull();
  });

  it('shows nothing until the setting is known, then shows, so it never flashes', async () => {
    await build(undefined, false);
    // Everything about the hook is known, and the setting has not been read.
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(1);
    expect(banner()).toBeNull();
    await TestBed.inject(RepeatReadsService).load();
    await settle();
    expect(banner()).not.toBeNull();
  });

  it('shows nothing when the setting cannot be read at all', async () => {
    await build((b) => b.failures.add('get_repeat_reads_setting'));
    expect(banner()).toBeNull();
  });

  it('shows nothing when the backend has no such command', async () => {
    // An older backend rejects the call. That must not blank the page, and must not accuse.
    await build((b) => b.failures.add('lumen_identity_hook_status'));
    expect(banner()).toBeNull();
  });

  it('shows nothing when the backend answers with something that is not a status', async () => {
    // Nothing is claimed from a reply that lacks either answer, however much of one it has.
    for (const answer of [
      undefined,
      null,
      'registered',
      {},
      { registered: 'no' },
      { registered: false },
      { servedElsewhere: false },
    ]) {
      TestBed.resetTestingModule();
      await build((b) => b.responses.set('lumen_identity_hook_status', answer));
      expect(banner(), JSON.stringify(answer)).toBeNull();
    }
  });

  it('shows nothing when it cannot tell whether Setup is needed', async () => {
    await build((b) => b.failures.add('lumen_setup_needed'));
    expect(banner()).toBeNull();
    TestBed.resetTestingModule();
    // A backend that answers with something other than a yes or a no is no better.
    await build((b) => b.responses.set('lumen_setup_needed', undefined));
    expect(banner()).toBeNull();
  });

  it('has no box of its own, so hidden it adds no gap to the page it sits in', async () => {
    // Both pages lay their children out in a flex column with a gap, and a host with a box is a
    // flex item even when it is empty: an empty one pushed the tab bar down by a whole gap.
    await build((b) => b.responses.set('lumen_identity_hook_status', REGISTERED));
    expect(banner()).toBeNull();
    expect(getComputedStyle(fixture.nativeElement as HTMLElement).display).toBe('contents');
  });

  // ── the button ─────────────────────────────────────────────────────────────

  it('takes the user to Setup, which runs its steps when it opens', async () => {
    await build();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    button().click();
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith(['/setup']);
  });

  it('leaves running Setup to the Setup page', async () => {
    await build();
    vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    button().click();
    await settle();
    expect(bridge.countOf('lumen_run_setup')).toBe(0);
  });

  // ── when it clears ─────────────────────────────────────────────────────────

  it('is asked again when the window gets focus, and goes once the hook is registered', async () => {
    await build();
    expect(banner()).not.toBeNull();
    bridge.responses.set('lumen_identity_hook_status', REGISTERED);
    focusWindow();
    await settle();
    expect(banner()).toBeNull();
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(2);
  });

  it('stays when the window gets focus and the hook is still missing', async () => {
    await build();
    focusWindow();
    await settle();
    expect(banner()).not.toBeNull();
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(2);
  });

  it('goes when the question fails after it, rather than stay on a guess', async () => {
    await build();
    bridge.failures.add('lumen_identity_hook_status');
    focusWindow();
    await settle();
    expect(banner()).toBeNull();
  });

  it('does not ask again on focus when it has nothing to show', async () => {
    await build((b) => b.responses.set('lumen_identity_hook_status', REGISTERED));
    focusWindow();
    focusWindow();
    await settle();
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(1);
    expect(bridge.countOf('lumen_setup_needed')).toBe(1);
  });

  it('stops listening for focus with the page it was on', async () => {
    await build();
    fixture.destroy();
    focusWindow();
    await Promise.resolve();
    expect(bridge.countOf('lumen_identity_hook_status')).toBe(1);
  });

  it('drops an answer that a later one has overtaken', async () => {
    await build();
    expect(banner()).not.toBeNull();

    // From here the status answers only when the test lets it, in the order it chooses.
    const answers: Array<(s: IdentityHookStatus) => void> = [];
    const answer = bridge.invoke.bind(bridge);
    bridge.invoke = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
      cmd === 'lumen_identity_hook_status'
        ? new Promise<T>((resolve) => answers.push((s) => resolve(s as T)))
        : answer<T>(cmd, args);

    focusWindow();
    focusWindow();
    await settle();
    expect(answers).toHaveLength(2);

    // The later question is answered first, and the earlier one after it with what was true
    // before: the prompt must not come back for it.
    answers[1](REGISTERED);
    await settle();
    expect(banner()).toBeNull();
    answers[0](MISSING);
    await settle();
    expect(banner()).toBeNull();
  });

  it('goes when repeat reads are switched off, and comes back when they are switched on', async () => {
    await build();
    const rr = TestBed.inject(RepeatReadsService);
    bridge.responses.set('set_repeat_reads_setting', setting({ on: false, source: 'file' }));
    await rr.set(false);
    await settle();
    expect(banner()).toBeNull();
    bridge.responses.set('set_repeat_reads_setting', setting({ on: true, source: 'file' }));
    await rr.set(true);
    await settle();
    expect(banner()).not.toBeNull();
  });
});
