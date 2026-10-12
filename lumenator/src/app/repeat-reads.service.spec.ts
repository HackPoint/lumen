import { TestBed } from '@angular/core/testing';
import { RepeatReadsService } from './repeat-reads.service';
import { TauriBridge } from './tauri-bridge';
import { FakeTauriBridge } from './tauri-bridge.fake';
import type { RepeatReadsSetting } from './components/index';

/**
 * The service says what the settings file holds, and says so when it cannot tell. It
 * never answers for the server: an env value in a Claude Code MCP config outranks the file.
 */
describe('RepeatReadsService', () => {
  const FILE = '/home/me/.lumen/lumen_settings.json';
  let bridge: FakeTauriBridge;
  let svc: RepeatReadsService;

  function setting(over: Partial<RepeatReadsSetting> = {}): RepeatReadsSetting {
    return { on: true, source: 'default', problem: null, file: FILE, ...over };
  }

  function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
    let resolve!: (v: T) => void;
    const promise = new Promise<T>((r) => {
      resolve = r;
    });
    return { promise, resolve };
  }

  async function ticks(): Promise<void> {
    for (let i = 0; i < 6; i++) await Promise.resolve();
  }

  beforeEach(() => {
    bridge = new FakeTauriBridge();
    TestBed.configureTestingModule({ providers: [{ provide: TauriBridge, useValue: bridge }] });
    svc = TestBed.inject(RepeatReadsService);
  });

  afterEach(() => TestBed.resetTestingModule());

  // ── reading ────────────────────────────────────────────────────────────────

  it('knows nothing until it has read the file', () => {
    expect(svc.setting()).toBeNull();
    expect(svc.readError()).toBeNull();
    expect(svc.writeError()).toBeNull();
    expect(svc.saving()).toBe(false);
  });

  it('publishes what the settings file holds', async () => {
    bridge.responses.set('get_repeat_reads_setting', setting({ on: false, source: 'file' }));
    await svc.load();
    expect(svc.setting()).toEqual(setting({ on: false, source: 'file' }));
    expect(svc.readError()).toBeNull();
  });

  it('says why it could not read, and claims no state', async () => {
    bridge.failures.add('get_repeat_reads_setting');
    await svc.load();
    expect(svc.readError()).toContain('fake: get_repeat_reads_setting failed');
    expect(svc.setting()).toBeNull();
  });

  it('carries a rejection that is a plain string, as Tauri sends it, unchanged', async () => {
    bridge.getRepeatReadsSetting = () => Promise.reject('cannot tell where the ledger lives');
    await svc.load();
    expect(svc.readError()).toBe('cannot tell where the ledger lives');
  });

  it('forgets a good reading when the next one fails', async () => {
    bridge.responses.set('get_repeat_reads_setting', setting());
    await svc.load();
    bridge.failures.add('get_repeat_reads_setting');
    await svc.load();
    expect(svc.setting()).toBeNull();
    expect(svc.readError()).not.toBeNull();
  });

  it('clears the error when a later read works', async () => {
    bridge.failures.add('get_repeat_reads_setting');
    await svc.load();
    bridge.failures.delete('get_repeat_reads_setting');
    bridge.responses.set('get_repeat_reads_setting', setting());
    await svc.load();
    expect(svc.readError()).toBeNull();
    expect(svc.setting()).toEqual(setting());
  });

  it('forgets an old write error when it reads again, so a reopened window does not repeat it', async () => {
    bridge.failures.add('set_repeat_reads_setting');
    await svc.set(false);
    expect(svc.writeError()).not.toBeNull();
    bridge.responses.set('get_repeat_reads_setting', setting());
    await svc.load();
    expect(svc.writeError()).toBeNull();
  });

  it.each([undefined, null, 'on', {}, { on: 'yes' }])(
    'does not take %j for a setting',
    async (payload) => {
      bridge.responses.set('get_repeat_reads_setting', payload);
      await svc.load();
      expect(svc.setting()).toBeNull();
      expect(svc.readError()).toContain('not a setting');
    },
  );

  // ── writing ────────────────────────────────────────────────────────────────

  it('sends the choice and publishes what the file says afterwards', async () => {
    bridge.responses.set('set_repeat_reads_setting', setting({ on: false, source: 'file' }));
    const ok = await svc.set(false);
    expect(bridge.lastArgsOf('set_repeat_reads_setting')).toEqual({ on: false });
    expect(ok).toBe(true);
    expect(svc.setting()).toEqual(setting({ on: false, source: 'file' }));
    expect(svc.writeError()).toBeNull();
    expect(svc.saving()).toBe(false);
  });

  it('is saving while the write is in flight, and not after', async () => {
    const write = deferred<RepeatReadsSetting>();
    bridge.setRepeatReadsSetting = () => write.promise;
    const done = svc.set(false);
    await ticks();
    expect(svc.saving()).toBe(true);
    write.resolve(setting({ on: false, source: 'file' }));
    await done;
    expect(svc.saving()).toBe(false);
  });

  it('is false, with the reason, when the write fails, and the setting stays as it was', async () => {
    bridge.responses.set('get_repeat_reads_setting', setting());
    await svc.load();
    bridge.failures.add('set_repeat_reads_setting');
    const ok = await svc.set(false);
    expect(ok).toBe(false);
    expect(svc.writeError()).toContain('fake: set_repeat_reads_setting failed');
    expect(svc.setting()).toEqual(setting());
    expect(svc.saving()).toBe(false);
  });

  it('forgets the last write error as the next write starts, not when it ends', async () => {
    bridge.failures.add('set_repeat_reads_setting');
    await svc.set(false);
    expect(svc.writeError()).not.toBeNull();
    bridge.failures.delete('set_repeat_reads_setting');
    const write = deferred<RepeatReadsSetting>();
    bridge.setRepeatReadsSetting = () => write.promise;
    const done = svc.set(false);
    await ticks();
    expect(svc.writeError()).toBeNull();
    write.resolve(setting({ on: false, source: 'file' }));
    await done;
    expect(svc.writeError()).toBeNull();
  });

  it.each([
    [false, true, 'says repeat reads are on, not off'],
    [true, false, 'says repeat reads are off, not on'],
  ])(
    'is not a success when asked for %j and the file afterwards says %j',
    async (asked, after, words) => {
      bridge.responses.set('set_repeat_reads_setting', setting({ on: after, source: 'file' }));
      const ok = await svc.set(asked);
      expect(ok).toBe(false);
      expect(svc.writeError()).toContain(words);
      expect(svc.setting()?.on).toBe(after);
    },
  );

  it('is not a success when the file cannot be read back', async () => {
    bridge.responses.set(
      'set_repeat_reads_setting',
      setting({ on: false, source: 'file', problem: 'not JSON: expected value at line 1' }),
    );
    const ok = await svc.set(false);
    expect(ok).toBe(false);
    expect(svc.writeError()).toContain('not JSON: expected value at line 1');
  });

  it('says so when the answer to a write is not a setting', async () => {
    bridge.responses.set('set_repeat_reads_setting', undefined);
    const ok = await svc.set(false);
    expect(ok).toBe(false);
    expect(svc.writeError()).toContain('not a setting');
  });

  // ── ordering ───────────────────────────────────────────────────────────────

  it('lets a read wait for the write in flight, so it cannot show the state from before it', async () => {
    const write = deferred<RepeatReadsSetting>();
    bridge.setRepeatReadsSetting = () => write.promise;
    bridge.responses.set('get_repeat_reads_setting', setting({ on: false, source: 'file' }));
    const writing = svc.set(false);
    const reading = svc.load();
    await ticks();
    expect(bridge.countOf('get_repeat_reads_setting')).toBe(0);
    write.resolve(setting({ on: false, source: 'file' }));
    await Promise.all([writing, reading]);
    expect(bridge.countOf('get_repeat_reads_setting')).toBe(1);
    expect(svc.setting()?.on).toBe(false);
  });

  it('lets a write wait for the read in flight, so that read cannot land after it and undo it', async () => {
    const read = deferred<RepeatReadsSetting>();
    bridge.getRepeatReadsSetting = () => read.promise;
    bridge.responses.set('set_repeat_reads_setting', setting({ on: false, source: 'file' }));
    const reading = svc.load();
    const writing = svc.set(false);
    await ticks();
    expect(bridge.countOf('set_repeat_reads_setting')).toBe(0);
    read.resolve(setting());
    await Promise.all([reading, writing]);
    expect(bridge.countOf('set_repeat_reads_setting')).toBe(1);
    expect(svc.setting()?.on).toBe(false);
  });
});
