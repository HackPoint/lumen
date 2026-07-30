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
});
