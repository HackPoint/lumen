// A release's evidence from the app as it was installed: drive the INSTALLED app, open its main
// window, and keep a screenshot of every tab and what the screens say. Linux goes through
// WebDriver (tauri-driver, WebKitWebDriver). Windows goes through the DevTools protocol:
// msedgedriver attaches only to a debug build, and WebView2 opens a debugging port for any app
// started with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, so the published build is what is captured.
// scripts/evidence/macos.sh does macOS through the accessibility API. It launches the installed
// app on the runner's real profile, so it runs only on a GitHub-hosted runner.
//
// usage: node e2e/evidence.mjs <installed app> <out dir> <label>
// writes: [setup-<label>.png], home-, optimizer-, details-, settings-<label>.png, and
//         app-<label>.json (the screens' text, the version on Settings, the Repeat reads switch)

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { Driver, Session } from './webdriver.mjs';

const [app, out, label] = process.argv.slice(2);
if (!app || !out || !label) {
  console.error('usage: node e2e/evidence.mjs <installed app> <out dir> <label>');
  process.exit(2);
}
if (!(process.env.GITHUB_ACTIONS === 'true' && process.env.RUNNER_ENVIRONMENT === 'github-hosted')) {
  console.error('refusing: this drives the installed app on the real profile; GitHub-hosted runners only');
  process.exit(2);
}
fs.mkdirSync(out, { recursive: true });

/**
 * One way to drive the page: `run(js, ...args)` evaluates a function body in the main window's
 * page and returns its value, `png()` is the page as a picture.
 */
async function webdriver() {
  const driver = await Driver.start({ ...process.env });
  const s = await Session.launch(driver, app);
  // The window loading "/" is the main one; the panel loads "/panel".
  const deadline = Date.now() + 30_000;
  find: for (;;) {
    for (const h of await s.handles()) {
      await s.switchTo(h);
      if ((await s.run('return location.pathname')) === '/') break find;
    }
    if (Date.now() > deadline) throw new Error('no window has "/" loaded');
    await sleep(250);
  }
  return {
    run: (body, ...args) => s.run(body, ...args),
    png: () => s.screenshot(),
    close: async () => {
      await s.quit().catch(e => console.error(`quit: ${e}`));
      await driver.stop();
    },
  };
}

async function devtools() {
  const port = 9333;
  const child = spawn(app, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
    stdio: 'ignore',
  });
  let target = null;
  const deadline = Date.now() + 60_000;
  while (!target) {
    if (Date.now() > deadline) throw new Error(`no page on the DevTools port ${port} within 60 s`);
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      target = list.find(t => t.type === 'page' && new URL(t.url).pathname === '/') ?? null;
    } catch {
      // Not listening yet.
    }
    if (!target) await sleep(500);
  }
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error(`cannot connect to ${target.webSocketDebuggerUrl}`));
  });
  let next = 0;
  const pending = new Map();
  ws.onmessage = e => {
    const m = JSON.parse(e.data);
    const done = pending.get(m.id);
    if (!done) return;
    pending.delete(m.id);
    if (m.error) done.reject(new Error(JSON.stringify(m.error)));
    else done.resolve(m.result);
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++next;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
  return {
    run: async (body, ...args) => {
      const expression = `(async function () { ${body} }).apply(null, ${JSON.stringify(args)})`;
      const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(`in the page: ${JSON.stringify(r.exceptionDetails)}`);
      return r.result.value ?? null;
    },
    png: async () => Buffer.from((await send('Page.captureScreenshot', { format: 'png' })).data, 'base64'),
    close: async () => {
      ws.close();
      child.kill();
    },
  };
}

const page = process.platform === 'win32' ? await devtools() : await webdriver();

/** Poll `body` until it returns something other than null, or fail after `ms`. */
async function until(what, body, ms, ...args) {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await page.run(body, ...args);
    if (v !== null && v !== undefined && v !== false) return v;
    if (Date.now() > deadline) throw new Error(`timed out after ${ms} ms waiting for ${what}`);
    await sleep(250);
  }
}

async function shoot(name) {
  const file = path.join(out, `${name}-${label}.png`);
  fs.writeFileSync(file, await page.png());
  const size = fs.statSync(file).size;
  if (size <= 1_000) throw new Error(`${file} holds no picture (${size} bytes)`);
  console.log(`screenshot: ${file} (${size} bytes)`);
}

const TEXT = `return document.querySelector(arguments[0])?.textContent.replace(/\\s+/g, ' ').trim() || null;`;
const said = { label, app, driver: process.platform === 'win32' ? 'DevTools protocol' : 'WebDriver' };
try {
  // A healthy launch keeps the window closed: open it, as the tray menu's Open Lumen does.
  await page.run(`return window.__TAURI_INTERNALS__.invoke('plugin:window|show', { label: 'main' })`);
  // A fresh install opens on Setup, which runs as it loads: keep that screen, then Open Lumen,
  // as a user does.
  const where = await until('Home or Setup', `
    if (document.querySelector('lumen-savings-headline')) return 'home';
    const done = [...document.querySelectorAll('button.setup__btn')].find(b => b.textContent.trim() === 'Open Lumen');
    return done && !done.disabled ? 'setup' : null;`, 60_000);
  if (where === 'setup') {
    await sleep(1_500);
    await shoot('setup');
    said.setup = await page.run(`return document.body.innerText.slice(0, 2000)`);
    await page.run(`[...document.querySelectorAll('button.setup__btn')].find(b => b.textContent.trim() === 'Open Lumen').click(); return true;`);
  }
  // Home: the savings headline, or what it says when nothing is recorded yet.
  said.home = await until('the savings headline', TEXT, 30_000, 'lumen-savings-headline');
  await sleep(1_500);
  await shoot('home');
  for (const tab of ['optimizer', 'details', 'settings']) {
    await page.run(`document.querySelector('a.tab-nav__tab[href="/' + arguments[0] + '"]').click(); return true;`, tab);
    await until(`the ${tab} tab`,
      `return document.querySelector('a.tab-nav__tab[href="/' + arguments[0] + '"]')?.classList.contains('is-active') || null;`,
      15_000, tab);
    await sleep(1_000);
    await shoot(tab);
  }
  said.version = await page.run(TEXT, 'section.about .about__version');
  said.repeatReads = await page.run(`
    const sec = document.querySelector('section.repeat');
    const sw = sec?.querySelector('input[type=checkbox], [role=switch]');
    if (!sw) return null;
    return sw.checked ?? sw.getAttribute('aria-checked');`);
} finally {
  await page.close();
}
fs.writeFileSync(path.join(out, `app-${label}.json`), JSON.stringify(said, null, 2));
console.log(JSON.stringify(said));
