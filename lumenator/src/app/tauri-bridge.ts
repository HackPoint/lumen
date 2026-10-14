import { Injectable } from '@angular/core';
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { moveWindow, Position } from '@tauri-apps/plugin-positioner';
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { Observable } from 'rxjs';
import type { RepeatReadsSetting } from './components/index';
import type { BillingMode, BillingModeSetting } from './billing-mode.service';

/**
 * The whole surface Lumen uses from the Tauri runtime, behind one injectable.
 *
 * Everything in `@tauri-apps/api` throws or hangs outside a Tauri window, so
 * importing it directly into a service makes that service impossible to
 * instantiate in a test or in a plain browser. Routing it through a provider
 * means tests and Playwright can swap in `FakeTauriBridge` and drive the whole
 * app deterministically.
 *
 * Keep this class free of logic — it is a boundary, not a place for behaviour.
 */
@Injectable({ providedIn: 'root' })
export class TauriBridge {
  /** Backend event stream, as an Observable of raw JSON payload strings. */
  listen$(event: string): Observable<string> {
    return new Observable<string>((subscriber) => {
      const unlisten = listen(event, (e) => subscriber.next(e.payload as string));
      return () => {
        void unlisten.then((fn) => fn());
      };
    });
  }

  /** Call a `#[tauri::command]`. */
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(cmd, args);
  }

  /**
   * The version this build was made as: the app's `PackageInfo`, which Tauri fills from
   * `tauri.conf.json` when the app is compiled. The tray menu's version line is made from the
   * same value, so the window and the tray cannot disagree.
   */
  getVersion(): Promise<string> {
    return getVersion();
  }

  /** The repeat-reads switch as the settings file holds it (never the server's effective mode). */
  getRepeatReadsSetting(): Promise<RepeatReadsSetting> {
    return this.invoke<RepeatReadsSetting>('get_repeat_reads_setting');
  }

  /** Write the switch to the settings file; resolves to what the file says afterwards. */
  setRepeatReadsSetting(on: boolean): Promise<RepeatReadsSetting> {
    return this.invoke<RepeatReadsSetting>('set_repeat_reads_setting', { on });
  }

  /** How the user pays for Claude Code, as the settings file holds it. */
  getBillingModeSetting(): Promise<BillingModeSetting> {
    return this.invoke<BillingModeSetting>('get_billing_mode_setting');
  }

  /** Write the billing mode to the settings file; resolves to what the file says afterwards. */
  setBillingModeSetting(mode: BillingMode): Promise<BillingModeSetting> {
    return this.invoke<BillingModeSetting>('set_billing_mode_setting', { mode });
  }

  isPermissionGranted(): Promise<boolean> {
    return isPermissionGranted();
  }

  requestPermission(): Promise<string> {
    return requestPermission();
  }

  sendNotification(options: { title: string; body: string }): void {
    sendNotification(options);
  }

  /** Park the popover under the tray icon. */
  moveWindowToTray(): Promise<void> {
    return moveWindow(Position.TrayBottomCenter);
  }
}
