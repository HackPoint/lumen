import { Observable, Subject } from 'rxjs';
import type { TauriBridge } from './tauri-bridge';

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
