import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Panel } from './panel';
import { SessionService } from '../../session.service';
import { TauriBridge } from '../../tauri-bridge';
import { FakeTauriBridge } from '../../tauri-bridge.fake';

/**
 * The panel is the tray popover: a compact read on the same session state, in a
 * window that has to park itself under the tray icon.
 */
describe('Panel', () => {
  let fixture: ComponentFixture<Panel>;
  let bridge: FakeTauriBridge;

  function build(): Panel {
    bridge = new FakeTauriBridge();
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function turn(cacheRead: number): string {
    return JSON.stringify({
      type: 'event',
      turn: {
        session_id: 's1',
        model: 'claude-sonnet-4',
        input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: cacheRead,
        cache_creation_input_tokens: 0,
      },
    });
  }

  afterEach(() => TestBed.resetTestingModule());

  it('parks itself under the tray icon on open', () => {
    build();
    expect(bridge.moveWindowCalls).toBe(1);
  });

  it('opens even if positioning fails', () => {
    // moveWindow rejects outside a Tauri window; the panel must still render.
    bridge = new FakeTauriBridge();
    bridge.moveWindowToTray = () => Promise.reject(new Error('no tray'));
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    expect(() => fixture.detectChanges()).not.toThrow();
  });

  it('starts at zero fill', () => {
    const p = build();
    expect(p.fillPct()).toBe(0);
    expect(p.fmtFill()).toBe('0');
  });

  it('mirrors the session fill as a percentage of the window', () => {
    const p = build();
    p.s.setWindow(200_000);
    bridge.emit('daemon', turn(50_000));
    expect(p.fillPct()).toBe(25);
  });

  it('formats the fill and window with thousands separators', () => {
    const p = build();
    p.s.setWindow(200_000);
    bridge.emit('daemon', turn(50_000));
    expect(p.fmtFill()).toBe((50_000).toLocaleString());
    expect(p.fmtMax()).toBe((200_000).toLocaleString());
  });

  it('tracks a manual window override', () => {
    const p = build();
    p.s.setWindow(1_000_000);
    expect(p.fmtMax()).toBe((1_000_000).toLocaleString());
  });

  it('renders the current fill into the DOM', () => {
    const p = build();
    p.s.setWindow(200_000);
    bridge.emit('daemon', turn(100_000));
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain(p.fmtFill());
  });
  // ── Fault indicator ─────────────────────────────────────────────────────────
  //
  // The panel has no router links, so a fault surfaced here must hand off to the main
  // window. It is also the window people actually open — the report button spent a
  // release on a screen reachable only from a nav this window does not have.

  async function withCount(n: number): Promise<Panel> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_fault_count', n);
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    fixture.detectChanges();
    for (let i = 0; i < 4; i++) await Promise.resolve();
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  function text(): string {
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  }

  function faultButton(): HTMLButtonElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('.panel__faults');
  }

  it('asks for the fault count on open', async () => {
    await withCount(0);
    expect(bridge.countOf('get_fault_count')).toBe(1);
  });

  it('stays out of the way when there is nothing to act on', async () => {
    await withCount(0);
    expect(faultButton()).toBeNull();
  });

  it('surfaces a recorded fault with a count', async () => {
    await withCount(3);
    expect(faultButton()).not.toBeNull();
    expect(text()).toContain('3 faults recorded');
  });

  it('reads naturally for a single fault', async () => {
    await withCount(1);
    expect(text()).toContain('1 fault recorded');
    expect(text()).not.toContain('1 faults');
  });

  it('hands off to the main window rather than filing anything', async () => {
    await withCount(2);
    faultButton()!.click();
    await Promise.resolve();
    expect(bridge.countOf('show_main_window')).toBe(1);
    // The panel cannot show the body, so it must never be able to file.
    expect(bridge.countOf('file_fault_report')).toBe(0);
  });
  // ── Update notice ───────────────────────────────────────────────────────────
  //
  // The policy that matters is what does NOT notify: a patch bump, a version already
  // announced, or a disabled check. All three are decided in the backend, which returns
  // null — so these assert the UI does nothing at all with a null, and notifies exactly
  // once with a value.

  async function withUpdate(u: unknown): Promise<void> {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_fault_count', 0);
    bridge.responses.set('check_for_update', u);
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    fixture.detectChanges();
    for (let i = 0; i < 8; i++) await Promise.resolve();
    fixture.detectChanges();
  }

  it('asks for an update on open', async () => {
    await withUpdate(null);
    expect(bridge.countOf('check_for_update')).toBe(1);
  });

  it('says nothing and notifies nothing when there is no update', async () => {
    await withUpdate(null);
    expect(fixture.nativeElement.querySelector('.panel__update')).toBeNull();
    expect(bridge.notifications.length).toBe(0);
  });

  it('shows a minor release and notifies once', async () => {
    await withUpdate({ current: '1.5.0', latest: '1.6.0', bump: 'minor', url: 'https://example.test/v1.6.0' });

    const el = fixture.nativeElement.querySelector('.panel__update') as HTMLAnchorElement;
    expect(el).not.toBeNull();
    expect(el.textContent).toContain('Lumen 1.6.0 available');
    expect(el.getAttribute('href')).toBe('https://example.test/v1.6.0');

    expect(bridge.notifications.length).toBe(1);
    expect(bridge.notifications[0].title).toContain('1.6.0');
    expect(bridge.notifications[0].body).toContain('1.5.0');
    expect(bridge.notifications[0].body).toContain('minor');
  });

  it('labels a major release as major', async () => {
    await withUpdate({ current: '1.9.0', latest: '2.0.0', bump: 'major', url: 'https://example.test/v2.0.0' });
    expect(bridge.notifications[0].body).toContain('major');
  });

  it('does not notify when notification permission is refused, but still shows the notice', async () => {
    bridge = new FakeTauriBridge();
    bridge.permissionGranted = false;
    bridge.permissionAnswer = 'denied';
    bridge.responses.set('get_fault_count', 0);
    bridge.responses.set('check_for_update', { current: '1.5.0', latest: '1.6.0', bump: 'minor', url: 'https://example.test/x' });
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    fixture.detectChanges();
    for (let i = 0; i < 8; i++) await Promise.resolve();
    fixture.detectChanges();

    expect(bridge.notifications.length).toBe(0);
    expect(fixture.nativeElement.querySelector('.panel__update')).not.toBeNull();
  });

  it('survives a backend that cannot check', async () => {
    bridge = new FakeTauriBridge();
    bridge.responses.set('get_fault_count', 0);
    bridge.failures.add('check_for_update');
    TestBed.configureTestingModule({
      providers: [{ provide: TauriBridge, useValue: bridge }, SessionService],
    });
    fixture = TestBed.createComponent(Panel);
    expect(() => fixture.detectChanges()).not.toThrow();
    for (let i = 0; i < 8; i++) await Promise.resolve();
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.panel__update')).toBeNull();
  });
  // ── Fitting the window to the content ───────────────────────────────────────
  //
  // The popover is a fixed-size window whose card used to clip anything that did not fit,
  // so each conditional row pushed the next one under the bottom edge. With a fault row
  // and an update notice both present, the fault button was off-screen entirely — present
  // in the DOM, styled, and unreachable. The window has to follow the content.

  it('asks the window to fit its content on open', async () => {
    await withUpdate(null);
    expect(bridge.countOf('resize_panel')).toBeGreaterThanOrEqual(1);
    const args = bridge.lastArgsOf('resize_panel');
    expect(args).toBeDefined();
    expect(typeof args!['height']).toBe('number');
  });

  it('never asks for a height that would clip the card', async () => {
    await withUpdate({ current: '1.5.0', latest: '1.6.0', bump: 'minor', url: 'https://example.test/x' });
    const card = fixture.nativeElement.querySelector('.panel') as HTMLElement;
    const asked = bridge.lastArgsOf('resize_panel')!['height'] as number;
    // 16px is the card's inset, 8 on each side. Asking for less than the card needs is
    // exactly the bug: the difference is what gets cut off.
    expect(asked).toBeGreaterThanOrEqual(Math.ceil(card.getBoundingClientRect().height) + 16);
  });

  /**
   * The card is measured, not assumed: whatever height it reports, the window must be
   * asked for at least that much plus the 8px inset on each side.
   *
   * Deliberately not asserting `getComputedStyle(card).bottom === 'auto'` — jsdom does no
   * layout, so that passes here and reports `-3.2px` in a real browser. The assertion that
   * the card is content-sized rather than stretched lives in the Rust CSS test, which
   * reads the stylesheet instead of a computed value.
   */
  it('requests a height derived from the measured card, not a constant', async () => {
    await withUpdate({ current: '1.5.0', latest: '1.6.0', bump: 'minor', url: 'https://example.test/x' });
    const card = fixture.nativeElement.querySelector('.panel') as HTMLElement;
    const asked = bridge.lastArgsOf('resize_panel')!['height'] as number;
    expect(asked).toBe(Math.ceil(card.getBoundingClientRect().height) + 16);
  });
});
