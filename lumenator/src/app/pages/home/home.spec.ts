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
});
