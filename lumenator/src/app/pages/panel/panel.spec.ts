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
});
