import { Observable, Subject } from 'rxjs';
import type { TauriBridge } from './tauri-bridge';
import type { RepeatReadsSetting } from './components/index';
import type { BillingMode, BillingModeSetting } from './billing-mode.service';
import type { ModelPrices, PriceRow, PriceTable } from './prices';

/** The command behind `getVersion()` in `@tauri-apps/api/app`. */
export const APP_VERSION_CMD = 'plugin:app|version';

type Usd = [number, number, number, number, number];

function prices([input, output, cacheWrite5m, cacheWrite1h, cacheRead]: Usd): ModelPrices {
  return { input, output, cacheWrite5m, cacheWrite1h, cacheRead };
}

/**
 * One row of the table `lumen_prices` serves: `[input, output, 5-minute write, 1-hour write,
 * cache read]` in USD per million tokens, at standard speed and, for a model with fast mode, in
 * it.
 */
export function priceRow(model: string, standard: Usd, fast?: Usd): PriceRow {
  return {
    provider: 'anthropic',
    model,
    prices: prices(standard),
    fast: fast ? prices(fast) : null,
    note: '',
    source: 'https://platform.claude.com/docs/en/about-claude/pricing',
  };
}

/**
 * A canned `lumen_prices` response, for specs to seed with
 * `responses.set('lumen_prices', PRICE_TABLE)`. Test data in the backend's shape: the app
 * itself holds no price and asks the backend, which serves `lumen_core::rates::PRICES`.
 */
export const PRICE_TABLE: PriceTable = {
  asOf: '2026-10-13',
  models: [
    priceRow('claude-opus-4-8', [5, 25, 6.25, 10, 0.5], [10, 50, 12.5, 20, 1]),
    priceRow('claude-opus-5-5', [4, 20, 5, 8, 0.2], [8, 40, 10, 16, 0.4]),
    priceRow('claude-sonnet-4-6', [3, 15, 3.75, 6, 0.3]),
    priceRow('claude-sonnet-5-5', [2, 10, 2.5, 4, 0.1]),
    priceRow('claude-haiku-4-5', [1, 5, 1.25, 2, 0.1]),
    { ...priceRow('claude-haiku-5-5', [0, 0, 0, 0, 0]), prices: null, note: 'Priced by prompt length.' },
  ],
};

/**
 * Test double for {@link TauriBridge}.
 *
 * Shared by the unit specs and the Playwright e2e build so both drive the app
 * through the same seam. It records every invoke so tests can assert what the
 * frontend asked the backend to do, and exposes `emit()` to push daemon frames.
 */
export class FakeTauriBridge implements TauriBridge {
  /** Every invoke call, in order: [command, args]. */
  readonly calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];

  /** Canned responses per command. Set before the code under test runs. */
  readonly responses = new Map<string, unknown>();

  /** Commands that should reject, simulating "not in Tauri" or a DB error. */
  readonly failures = new Set<string>();

  readonly notifications: Array<{ title: string; body: string }> = [];

  permissionGranted = true;
  /** What requestPermission() resolves to when permission is not yet granted. */
  permissionAnswer = 'granted';

  private readonly streams = new Map<string, Subject<string>>();

  listen$(event: string): Observable<string> {
    return this.stream(event).asObservable();
  }

  /** Push a raw payload to everyone listening on `event`. */
  emit(event: string, payload: string): void {
    this.stream(event).next(payload);
  }

  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push({ cmd, args });
    if (this.failures.has(cmd)) {
      return Promise.reject(new Error(`fake: ${cmd} failed`));
    }
    return Promise.resolve(this.responses.get(cmd) as T);
  }

  /**
   * Stands in for `getVersion()`, which in the app is the `plugin:app|version` command. It is
   * recorded, seeded and failed like any other command, under that name:
   * `responses.set(APP_VERSION_CMD, '1.2.3')`.
   */
  getVersion(): Promise<string> {
    return this.invoke<string>(APP_VERSION_CMD);
  }

  getRepeatReadsSetting(): Promise<RepeatReadsSetting> {
    return this.invoke<RepeatReadsSetting>('get_repeat_reads_setting');
  }

  setRepeatReadsSetting(on: boolean): Promise<RepeatReadsSetting> {
    return this.invoke<RepeatReadsSetting>('set_repeat_reads_setting', { on });
  }

  getBillingModeSetting(): Promise<BillingModeSetting> {
    return this.invoke<BillingModeSetting>('get_billing_mode_setting');
  }

  setBillingModeSetting(mode: BillingMode): Promise<BillingModeSetting> {
    return this.invoke<BillingModeSetting>('set_billing_mode_setting', { mode });
  }

  isPermissionGranted(): Promise<boolean> {
    return Promise.resolve(this.permissionGranted);
  }

  requestPermission(): Promise<string> {
    return Promise.resolve(this.permissionAnswer);
  }

  sendNotification(options: { title: string; body: string }): void {
    this.notifications.push(options);
  }

  /** How many times the window was parked under the tray. */
  moveWindowCalls = 0;

  moveWindowToTray(): Promise<void> {
    this.moveWindowCalls++;
    return Promise.resolve();
  }

  // ── helpers for assertions ────────────────────────────────────────────────

  /** How many times `cmd` was invoked. */
  countOf(cmd: string): number {
    return this.calls.filter((c) => c.cmd === cmd).length;
  }

  /** Args of the most recent `cmd` invoke, or undefined if never called. */
  lastArgsOf(cmd: string): Record<string, unknown> | undefined {
    return this.calls.filter((c) => c.cmd === cmd).at(-1)?.args;
  }

  private stream(event: string): Subject<string> {
    let s = this.streams.get(event);
    if (!s) {
      s = new Subject<string>();
      this.streams.set(event, s);
    }
    return s;
  }
}
