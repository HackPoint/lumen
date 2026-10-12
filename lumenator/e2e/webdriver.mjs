// The W3C WebDriver commands the app tests use, over fetch, against tauri-driver.
//
// No client library: the tests need a dozen endpoints, and each is one HTTP call. tauri-driver
// starts the platform's own driver behind it — WebKitWebDriver on Linux, msedgedriver on Windows.
// macOS has no WebDriver for WKWebView, so there is none there.

import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';
import { setTimeout as sleep } from 'node:timers/promises';

// A call that gets no answer fails, naming itself, instead of holding the run until the CI job's
// own limit. Starting a session launches the app, so it is given longer than the rest.
const CALL_MS = 60_000;
const LAUNCH_MS = 120_000;
const ELEMENT = 'element-6066-11e4-a52e-4f735466cecf';

async function call(base, method, path, body) {
  const ms = method === 'POST' && path === '/session' ? LAUNCH_MS : CALL_MS;
  let text, res;
  try {
    res = await fetch(base + path, {
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

/** `n` distinct ports that nothing is listening on, as the OS hands them out. */
async function freePorts(n) {
  const servers = await Promise.all(
    Array.from({ length: n }, () => new Promise((resolve, reject) => {
      const s = net.createServer();
      s.once('error', reject);
      s.listen(0, '127.0.0.1', () => resolve(s));
    })),
  );
  const ports = servers.map(s => s.address().port);
  await Promise.all(servers.map(s => new Promise(resolve => s.close(resolve))));
  return ports;
}

/** Stop `child` and what it started, and return once it has exited. */
async function terminate(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { timeout: 30_000 });
  } else {
    try {
      process.kill(-child.pid, 'SIGTERM');
    } catch {
      // Already gone.
    }
  }
  try {
    await once(child, 'exit', { signal: AbortSignal.timeout(10_000) });
  } catch (e) {
    if (e?.name !== 'TimeoutError' && e?.name !== 'AbortError') throw e;
    if (process.platform === 'win32') {
      throw new Error(`tauri-driver ${child.pid} did not exit within 10 s of taskkill`);
    }
    try {
      process.kill(-child.pid, 'SIGKILL');
    } catch {
      // Gone between the timeout and now.
    }
    await once(child, 'exit', { signal: AbortSignal.timeout(10_000) });
  }
}

/** tauri-driver, started with the environment the app should run in. */
export class Driver {
  /** @param {NodeJS.ProcessEnv} env  inherited by the driver, and through it by the app */
  static async start(env) {
    // Ports of this driver's own. The defaults, 4444 and 4445, are fixed, and the previous
    // driver's msedgedriver can still hold 4445 for a moment after its tree was killed: the new
    // msedgedriver then fails to bind it and exits, while tauri-driver stays up on 4444 passing
    // every request on to a port nobody listens on.
    const [port, nativePort] = await freePorts(2);
    const args = ['--port', String(port), '--native-port', String(nativePort)];
    if (process.env.LUMEN_E2E_NATIVE_DRIVER) args.push('--native-driver', process.env.LUMEN_E2E_NATIVE_DRIVER);
    const child = spawn(process.env.TAURI_DRIVER ?? 'tauri-driver', args, {
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      // Its own process group on POSIX, so stopping it takes the app and the app's daemon
      // with it rather than leaving them holding the next run's ports.
      detached: process.platform !== 'win32',
    });
    const d = new Driver(child, `http://127.0.0.1:${port}`);
    const deadline = Date.now() + 20_000;
    try {
      for (;;) {
        if (child.exitCode !== null) {
          throw new Error(`tauri-driver exited with ${child.exitCode}:\n${d.output}`);
        }
        // Neither of these ends in an exit that is seen as one: tauri-driver exits 0 when it
        // cannot listen, and when its native driver cannot bind, it stays up and answers nothing.
        if (/can not listen to address|bind\(\) returned an error/.test(d.output)) {
          throw new Error(`tauri-driver on ports ${port}/${nativePort} could not bind:\n${d.output}`);
        }
        try {
          await fetch(d.base + '/status', { signal: AbortSignal.timeout(2_000) });
          return d;
        } catch {
          if (Date.now() > deadline) throw new Error(`tauri-driver never listened:\n${d.output}`);
          await sleep(200);
        }
      }
    } catch (e) {
      // A driver that did not come up must not be left holding its ports, or the run.
      await terminate(child).catch(() => {});
      throw e;
    }
  }

  constructor(child, base) {
    this.child = child;
    this.base = base;
    this.output = '';
    child.stdout.on('data', b => (this.output += b));
    child.stderr.on('data', b => (this.output += b));
  }

  /** Stop the driver and what it started, and return once the driver has exited. */
  async stop() {
    await terminate(this.child);
  }
}

export class Session {
  /** Launch the app at `application` through `driver`, and attach to it. */
  static async launch(driver, application, args = []) {
    const v = await call(driver.base, 'POST', '/session', {
      capabilities: { alwaysMatch: { 'tauri:options': { application, args } } },
    });
    return new Session(driver.base, v.sessionId);
  }

  constructor(base, id) {
    this.base = base;
    this.p = `/session/${id}`;
  }

  quit() {
    return call(this.base, 'DELETE', this.p);
  }

  handles() {
    return call(this.base, 'GET', `${this.p}/window/handles`);
  }

  switchTo(handle) {
    return call(this.base, 'POST', `${this.p}/window`, { handle });
  }

  /** Run `script` in the page; a returned promise is awaited. */
  run(script, ...args) {
    return call(this.base, 'POST', `${this.p}/execute/sync`, { script, args });
  }

  /**
   * Click the element matching `css`, once the page has rendered it: a window can be on its URL
   * before the page's script has drawn anything, and the first lookup then finds nothing.
   */
  async click(css) {
    await this.until(`${css} to be rendered`, 'return document.querySelector(arguments[0]) !== null', 15_000, css);
    const el = await call(this.base, 'POST', `${this.p}/element`, { using: 'css selector', value: css });
    await call(this.base, 'POST', `${this.p}/element/${el[ELEMENT]}/click`, {});
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
