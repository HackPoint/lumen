import { Injectable } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { Observable } from 'rxjs';

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

  isPermissionGranted(): Promise<boolean> {
    return isPermissionGranted();
  }

  requestPermission(): Promise<string> {
    return requestPermission();
  }

  sendNotification(options: { title: string; body: string }): void {
    sendNotification(options);
  }
}
