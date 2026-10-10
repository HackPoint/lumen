// The W3C WebDriver commands the app tests use, over fetch, against tauri-driver.
//
// No client library: the tests need a dozen endpoints, and each is one HTTP call. tauri-driver
// listens on 4444 and starts the platform's own driver behind it — WebKitWebDriver on Linux,
// msedgedriver on Windows. macOS has no WebDriver for WKWebView, so there is none there.

import { spawn, spawnSync } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

const BASE = 'http://127.0.0.1:4444';
// A call that gets no answer fails, naming itself, instead of holding the run until the CI job's
// own limit. Starting a session launches the app, so it is given longer than the rest.
const CALL_MS = 60_000;
const LAUNCH_MS = 120_000;
const ELEMENT = 'element-6066-11e4-a52e-4f735466cecf';

async function call(method, path, body) {
  const ms = method === 'POST' && path === '/session' ? LAUNCH_MS : CALL_MS;
  let text, res;
  try {
    res = await fetch(BASE + path, {
      method,
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(ms),
    });
    text = await res.text();
  } catch (e) {
    if (e?.name === 'TimeoutError') {
      throw new Error(`${method} ${path}: tauri-driver gave no answer in ${ms / 1000} s`);
    }
    throw e;
  }
  let value;
  try {
    value = JSON.parse(text).value;
  } catch {
    throw new Error(`${method} ${path}: HTTP ${res.status}, not JSON: ${text.slice(0, 300)}`);
  }
  if (!res.ok || value?.error) {
    throw new Error(`${method} ${path}: HTTP ${res.status} ${value?.error}: ${value?.message}`);
  }
  return value;
}

/** tauri-driver, started with the environment the app should run in. */
export class Driver {
  /** @param {NodeJS.ProcessEnv} env  inherited by the driver, and through it by the app */
  static async start(env) {
    const args = process.env.LUMEN_E2E_NATIVE_DRIVER
      ? ['--native-driver', process.env.LUMEN_E2E_NATIVE_DRIVER]
      : [];
    const child = spawn(process.env.TAURI_DRIVER ?? 'tauri-driver', args, {
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      // Its own process group on POSIX, so stopping it takes the app and the app's daemon
      // with it rather than leaving them holding the next run's ports.
      detached: process.platform !== 'win32',
    });
    const d = new Driver(child);
    const deadline = Date.now() + 20_000;
    for (;;) {
      if (child.exitCode !== null) {
        throw new Error(`tauri-driver exited with ${child.exitCode}:\n${d.output}`);
      }
      try {
        await fetch(BASE + '/status', { signal: AbortSignal.timeout(2_000) });
        return d;
      } catch {
        if (Date.now() > deadline) throw new Error(`tauri-driver never listened:\n${d.output}`);
        await sleep(200);
      }
    }
  }

  constructor(child) {
    this.child = child;
    this.output = '';
    child.stdout.on('data', b => (this.output += b));
    child.stderr.on('data', b => (this.output += b));
  }

  async stop() {
    const { child } = this;
    if (child.exitCode === null) {
      if (process.platform === 'win32') {
        spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { timeout: 30_000 });
      } else {
        try {
          process.kill(-child.pid, 'SIGTERM');
        } catch {
          // Already gone.
        }
      }
    }
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      try {
        await fetch(BASE + '/status', { signal: AbortSignal.timeout(2_000) });
      } catch {
        return;
      }
      await sleep(200);
    }
    if (process.platform !== 'win32') process.kill(-child.pid, 'SIGKILL');
  }
}

export class Session {
  /** Launch the app at `application` and attach to it. */
  static async launch(application, args = []) {
    const v = await call('POST', '/session', {
      capabilities: { alwaysMatch: { 'tauri:options': { application, args } } },
    });
    return new Session(v.sessionId);
  }

  constructor(id) {
    this.p = `/session/${id}`;
  }

  quit() {
    return call('DELETE', this.p);
  }

  handles() {
    return call('GET', `${this.p}/window/handles`);
  }

  switchTo(handle) {
    return call('POST', `${this.p}/window`, { handle });
  }

  /** Run `script` in the page; a returned promise is awaited. */
  run(script, ...args) {
    return call('POST', `${this.p}/execute/sync`, { script, args });
  }

  async click(css) {
    const el = await call('POST', `${this.p}/element`, { using: 'css selector', value: css });
    await call('POST', `${this.p}/element/${el[ELEMENT]}/click`, {});
  }

  /**
   * Run `script` until it returns something truthy, and return that. A script that throws
   * counts as not yet: the page may be mid-navigation.
   */
  async until(what, script, timeoutMs = 15_000, ...args) {
    const deadline = Date.now() + timeoutMs;
    let last;
    for (;;) {
      try {
        const v = await this.run(script, ...args);
        if (v) return v;
        last = v;
      } catch (e) {
        last = e.message;
      }
      if (Date.now() > deadline) {
        throw new Error(`timed out after ${timeoutMs} ms waiting for ${what}; last: ${JSON.stringify(last)}`);
      }
      await sleep(250);
    }
  }
}
