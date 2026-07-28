import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Setup } from './setup';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';

type StepStatus = 'Ok' | 'Warn' | 'Error' | 'Skip';

/**
 * The setup page is the first thing a new user sees, and the only UI for the
 * install/uninstall that rewrites their Claude Code config. It must report what
 * actually happened — including partial failure — rather than a cheerful green.
 */
describe('Setup', () => {
  let fixture: ComponentFixture<Setup>;
  let bridge: FakeTauriBridge;

  function step(id: string, status: StepStatus, detail = 'detail') {
    return { id, label: `Step ${id}`, status, detail };
  }

  /** Build the page; ngOnInit immediately runs setup. */
  async function build(seed?: (b: FakeTauriBridge) => void): Promise<Setup> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('lumen_run_setup', []);
    seed?.(bridge);
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: TauriBridge, useValue: bridge }],
    });
    fixture = TestBed.createComponent(Setup);
    fixture.detectChanges();
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  afterEach(() => TestBed.resetTestingModule());

  // ── run on open ────────────────────────────────────────────────────────────

  it('runs setup as soon as the page opens', async () => {
    await build();
    expect(bridge.countOf('lumen_run_setup')).toBe(1);
  });

  it('lands in the done phase with the reported steps', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('mcp', 'Ok'), step('hooks', 'Ok')]),
    );
    expect(s.phase()).toBe('done');
    expect(s.steps()).toHaveLength(2);
    expect(s.error()).toBeNull();
  });

  it('surfaces a backend failure instead of pretending it worked', async () => {
    const s = await build((b) => b.failures.add('lumen_run_setup'));
    expect(s.phase()).toBe('done');
    expect(s.error()).toContain('lumen_run_setup failed');
    expect(s.steps()).toHaveLength(0);
  });

  // ── status roll-up ─────────────────────────────────────────────────────────

  it('treats all-Ok as success', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Ok'), step('b', 'Ok')]),
    );
    expect(s.allOk).toBe(true);
    expect(s.hasError).toBe(false);
  });

  it('still counts a Warn as acceptable', async () => {
    // A warn means "worked, with a caveat" — e.g. Claude Code not currently
    // running. Blocking the user on that would be wrong.
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Ok'), step('b', 'Warn')]),
    );
    expect(s.allOk).toBe(true);
    expect(s.hasError).toBe(false);
  });

  it('does not call a run with a Skip step a success', async () => {
    // Skip means a step did not happen. Reporting that as all-clear would
    // overstate what was installed.
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Ok'), step('b', 'Skip')]),
    );
    expect(s.allOk).toBe(false);
  });

  it('reports an error when any step failed', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Ok'), step('b', 'Error')]),
    );
    expect(s.hasError).toBe(true);
    expect(s.allOk).toBe(false);
  });

  it('treats an empty step list as vacuously ok', async () => {
    const s = await build();
    expect(s.allOk).toBe(true);
    expect(s.hasError).toBe(false);
  });

  // ── icons ──────────────────────────────────────────────────────────────────

  it('gives every status a distinct icon', async () => {
    const s = await build();
    const icons = (['Ok', 'Warn', 'Error', 'Skip'] as StepStatus[]).map((st) => s.iconFor(st));
    expect(icons).toEqual(['✓', '!', '✕', '–']);
    expect(new Set(icons).size).toBe(4);
  });

  // ── uninstall ──────────────────────────────────────────────────────────────

  it('runs uninstall on demand and records the steps', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_uninstall', [step('mcp', 'Ok', 'Removed from ~/.claude.json')]),
    );
    await s.uninstall();
    fixture.detectChanges();
    expect(bridge.countOf('lumen_uninstall')).toBe(1);
    expect(s.steps()[0].detail).toContain('Removed');
    expect(s.phase()).toBe('done');
  });

  it('surfaces an uninstall failure', async () => {
    const s = await build((b) => b.failures.add('lumen_uninstall'));
    await s.uninstall();
    expect(s.error()).toContain('lumen_uninstall failed');
    expect(s.phase()).toBe('done');
  });

  it('clears prior results before re-running', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Error', 'boom')]),
    );
    expect(s.hasError).toBe(true);
    bridge.responses.set('lumen_run_setup', [step('a', 'Ok')]);
    await s.runSetup();
    expect(s.hasError).toBe(false);
    expect(s.error()).toBeNull();
  });

  // ── CLI install ────────────────────────────────────────────────────────────

  it('reports a successful CLI install with a tick', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_install_cli', [step('cli', 'Ok', 'Linked /usr/local/bin/lumen')]),
    );
    await s.installCli();
    expect(s.cliResult()).toBe('✓ Linked /usr/local/bin/lumen');
    expect(s.cliRunning()).toBe(false);
  });

  it('reports a failed CLI install with a cross, not a tick', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_install_cli', [step('cli', 'Error', 'Permission denied')]),
    );
    await s.installCli();
    expect(s.cliResult()).toBe('✕ Permission denied');
  });

  it('reports a thrown CLI install error rather than hanging', async () => {
    const s = await build((b) => b.failures.add('lumen_install_cli'));
    await s.installCli();
    expect(s.cliResult()).toContain('✕');
    expect(s.cliRunning()).toBe(false);
  });

  it('always clears the running flag, even on failure', async () => {
    const s = await build((b) => b.failures.add('lumen_install_cli'));
    await s.installCli();
    expect(s.cliRunning()).toBe(false);
  });

  it('leaves no result when the backend returns no steps', async () => {
    const s = await build((b) => b.responses.set('lumen_install_cli', []));
    await s.installCli();
    expect(s.cliResult()).toBeNull();
    expect(s.cliRunning()).toBe(false);
  });

  // ── navigation ─────────────────────────────────────────────────────────────

  it('navigates home when asked', async () => {
    const s = await build();
    const router = TestBed.inject(Router);
    const spy = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    s.goHome();
    expect(spy).toHaveBeenCalledWith(['/']);
  });

  // ── firefly state ──────────────────────────────────────────────────────────

  it('dims the firefly when a step failed', async () => {
    const s = await build((b) =>
      b.responses.set('lumen_run_setup', [step('a', 'Error')]),
    );
    expect(s.fireflyState()).toBe('soft');
  });

  it('keeps the firefly bright while running and on success', async () => {
    const s = await build((b) => b.responses.set('lumen_run_setup', [step('a', 'Ok')]));
    expect(s.fireflyState()).toBe('full');
  });

  // ── rendering ──────────────────────────────────────────────────────────────

  it('renders the step details it was given', async () => {
    await build((b) =>
      b.responses.set('lumen_run_setup', [step('mcp', 'Ok', 'lumen added to ~/.claude.json')]),
    );
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('lumen added to ~/.claude.json');
  });

  // ── launch at login ────────────────────────────────────────────────────────
  //
  // The toggle reflects the OS, not what the UI last asked for: a login item can
  // fail to register (policy, sandbox) and setup reports that as Warn, not Error,
  // so believing the request instead of the result would show a lie.

  it('reads the real login-item state after setup rather than assuming it', async () => {
    const s = await build((b) => b.responses.set('lumen_autostart_enabled', true));
    expect(bridge.countOf('lumen_autostart_enabled')).toBe(1);
    expect(s.autostart()).toBe(true);
  });

  it('shows the toggle off when setup could not register the login item', async () => {
    const s = await build((b) => {
      b.responses.set('lumen_run_setup', [step('autostart', 'Warn', 'Could not enable')]);
      b.responses.set('lumen_autostart_enabled', false);
    });
    expect(s.autostart()).toBe(false);
  });

  it('treats an unanswerable backend as off so the toggle still renders', async () => {
    const s = await build((b) => b.failures.add('lumen_autostart_enabled'));
    expect(s.autostart()).toBe(false);
  });

  it('turning the toggle on asks the backend to enable it', async () => {
    const s = await build((b) => {
      b.responses.set('lumen_autostart_enabled', false);
      b.responses.set('lumen_set_autostart', true);
    });
    await s.toggleAutostart();
    expect(bridge.lastArgsOf('lumen_set_autostart')).toEqual({ enable: true });
    expect(s.autostart()).toBe(true);
  });

  it('turning the toggle off asks the backend to disable it', async () => {
    const s = await build((b) => {
      b.responses.set('lumen_autostart_enabled', true);
      b.responses.set('lumen_set_autostart', false);
    });
    await s.toggleAutostart();
    expect(bridge.lastArgsOf('lumen_set_autostart')).toEqual({ enable: false });
    expect(s.autostart()).toBe(false);
  });

  it('believes the state the OS reports, not the state it asked for', async () => {
    // Asked to enable, but the OS still says off — the toggle must not lie.
    const s = await build((b) => {
      b.responses.set('lumen_autostart_enabled', false);
      b.responses.set('lumen_set_autostart', false);
    });
    await s.toggleAutostart();
    expect(s.autostart()).toBe(false);
  });

  it('surfaces a toggle failure and re-reads the actual state', async () => {
    const s = await build((b) => {
      b.responses.set('lumen_autostart_enabled', false);
      b.failures.add('lumen_set_autostart');
    });
    const before = bridge.countOf('lumen_autostart_enabled');
    await s.toggleAutostart();
    expect(s.autostartError()).toContain('lumen_set_autostart failed');
    expect(bridge.countOf('lumen_autostart_enabled')).toBe(before + 1);
    expect(s.autostartBusy()).toBe(false);
  });

  it('clears the busy flag even when the toggle succeeds', async () => {
    const s = await build((b) => b.responses.set('lumen_set_autostart', true));
    await s.toggleAutostart();
    expect(s.autostartBusy()).toBe(false);
  });

  it('re-reads the login-item state after uninstall removed it', async () => {
    const s = await build((b) => b.responses.set('lumen_autostart_enabled', true));
    expect(s.autostart()).toBe(true);
    // Uninstall disables it, so the next read reports off.
    bridge.responses.set('lumen_uninstall', [step('autostart', 'Ok')]);
    bridge.responses.set('lumen_autostart_enabled', false);
    await s.uninstall();
    expect(s.autostart()).toBe(false);
  });

  it('renders the toggle with a label a user can act on', async () => {
    await build((b) => b.responses.set('lumen_autostart_enabled', true));
    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain('Start Lumen at login');
    const box = el.querySelector<HTMLInputElement>('.setup__toggle-input');
    expect(box).not.toBeNull();
    expect(box!.type).toBe('checkbox');
    expect(box!.checked).toBe(true);
  });
});
