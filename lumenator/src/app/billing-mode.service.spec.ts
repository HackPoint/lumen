import { TestBed } from '@angular/core/testing';
import { BillingModeService } from './billing-mode.service';
import type { BillingModeSetting } from './billing-mode.service';
import { TauriBridge } from './tauri-bridge';
import { FakeTauriBridge } from './tauri-bridge.fake';

describe('BillingModeService', () => {
  let bridge: FakeTauriBridge;
  let svc: BillingModeService;
  const FILE = '/Users/me/.lumen/lumen_settings.json';

  function setting(over: Partial<BillingModeSetting> = {}): BillingModeSetting {
    return { mode: null, problem: null, file: FILE, ...over };
  }

  beforeEach(() => {
    bridge = new FakeTauriBridge();
    TestBed.configureTestingModule({ providers: [{ provide: TauriBridge, useValue: bridge }] });
    svc = TestBed.inject(BillingModeService);
  });

  afterEach(() => TestBed.resetTestingModule());

  it('knows nothing, and calls nothing unasked, before the first read', () => {
    expect(svc.setting()).toBeNull();
    expect(svc.unasked()).toBe(false);
  });

  it('is unasked only when the file was read, holds no answer and has no problem', async () => {
    bridge.responses.set('get_billing_mode_setting', setting());
    await svc.load();
    expect(svc.unasked()).toBe(true);

    bridge.responses.set('get_billing_mode_setting', setting({ mode: 'api' }));
    await svc.load();
    expect(svc.unasked()).toBe(false);

    bridge.responses.set('get_billing_mode_setting', setting({ problem: 'not JSON: x' }));
    await svc.load();
    expect(svc.unasked()).toBe(false);
  });

  it('round-trips a choice through the backend', async () => {
    bridge.responses.set('set_billing_mode_setting', setting({ mode: 'subscription' }));
    expect(await svc.set('subscription')).toBe(true);
    expect(bridge.lastArgsOf('set_billing_mode_setting')).toEqual({ mode: 'subscription' });
    expect(svc.setting()?.mode).toBe('subscription');
    expect(svc.writeError()).toBeNull();
    expect(svc.saving()).toBe(false);
  });

  it('reports a failed write and keeps what it knew', async () => {
    bridge.responses.set('get_billing_mode_setting', setting({ mode: 'api' }));
    await svc.load();
    bridge.failures.add('set_billing_mode_setting');
    expect(await svc.set('subscription')).toBe(false);
    expect(svc.writeError()).toBe('fake: set_billing_mode_setting failed');
    expect(svc.setting()?.mode).toBe('api');
  });

  it('reports a read that failed, and a later read clears it', async () => {
    bridge.failures.add('get_billing_mode_setting');
    await svc.load();
    expect(svc.readError()).toBe('fake: get_billing_mode_setting failed');
    expect(svc.setting()).toBeNull();
    bridge.failures.delete('get_billing_mode_setting');
    bridge.responses.set('get_billing_mode_setting', setting({ mode: 'api' }));
    await svc.load();
    expect(svc.readError()).toBeNull();
    expect(svc.setting()?.mode).toBe('api');
  });

  it('refuses a reply that is not a billing setting, rather than guessing from it', async () => {
    for (const bad of [
      null,
      'api',
      { mode: 'pro', problem: null, file: FILE },
      { mode: null, problem: 3, file: FILE },
      { mode: null, problem: null },
    ]) {
      bridge.responses.set('get_billing_mode_setting', bad);
      await svc.load();
      expect(svc.setting(), JSON.stringify(bad)).toBeNull();
      expect(svc.readError()).toBe('the app answered with something that is not a billing setting');
    }
  });

  it('describes a rejection that is not an Error by its text', async () => {
    bridge.getBillingModeSetting = () => Promise.reject('no ledger');
    await svc.load();
    expect(svc.readError()).toBe('no ledger');
  });

  it('runs a read queued behind a write after it, so the read cannot be overtaken', async () => {
    const order: string[] = [];
    let finish!: (v: BillingModeSetting) => void;
    bridge.setBillingModeSetting = () => {
      order.push('write');
      return new Promise<BillingModeSetting>((r) => (finish = r));
    };
    bridge.getBillingModeSetting = () => {
      order.push('read');
      return Promise.resolve(setting({ mode: 'api' }));
    };
    const write = svc.set('api');
    const read = svc.load();
    await Promise.resolve();
    expect(order).toEqual(['write']);
    finish(setting({ mode: 'api' }));
    await write;
    await read;
    expect(order).toEqual(['write', 'read']);
  });
});
