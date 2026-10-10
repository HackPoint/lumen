// The built app, launched through WebDriver, showing a ledger the real lumen-mcp wrote.
//
// What the page specs cannot reach: the binary's own startup, whether it opens its window,
// and the Optimizer and Hotspots screens rendering figures that came out of SQLite through
// the Tauri commands rather than from a test double.
//
// After `bash build-sidecar.sh` and `pnpm tauri build --debug --no-bundle`, from lumenator/:
//   Linux:          xvfb-run -a node --test 'e2e/*.e2e.mjs'
//   Windows, in CI: node --test 'e2e/*.e2e.mjs', unelevated, with LUMEN_E2E_NATIVE_DRIVER
//                   naming an msedgedriver.exe of the WebView2 runtime's version. Elevated, no
//                   session attaches; ci.yml says why.
// macOS has no WebDriver for its web view, so the app cannot be driven there this way.
//
// On Linux every test gives the app a home and data directory of its own, so nothing here
// reads or writes the ledger, the hooks or the settings of the machine it runs on. Windows
// takes those directories from the user's profile whatever the environment says, so there the
// tests refuse to run outside CI, and on a runner use its profile, emptied before each test.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { Driver, Session } from './webdriver.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const EXE = process.platform === 'win32' ? '.exe' : '';
const APP = process.env.LUMEN_E2E_APP ?? path.join(ROOT, 'target', 'debug', `Lumen${EXE}`);
const MCP = process.env.LUMEN_E2E_MCP ?? path.join(ROOT, 'target', 'release', `lumen-mcp${EXE}`);
const SETUP_RS = path.join(ROOT, 'lumenator', 'src-tauri', 'src', 'setup.rs');
const LIB_RS = path.join(ROOT, 'lumenator', 'src-tauri', 'src', 'lib.rs');

// How long `verify_tray_presence` in lib.rs takes to give up on the tray: 500 + 1,500 + 4,000 ms
// after RunEvent::Ready.
const PRESENCE_CHECKS_MS = 6_000;

const WINDOWS = process.platform === 'win32';

if (!(process.platform === 'linux' || (WINDOWS && process.env.CI))) {
  // Elsewhere the app's data and home directories come from the OS rather than from the
  // environment, so a run could not be kept off the real ledger.
  throw new Error('these tests run on Linux, and on Windows only in CI; see the header');
}

/** A home and data directory of the test's own, holding the marker that Setup has run. */
function sandbox() {
  if (WINDOWS) return runnerProfile();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lumen-e2e-'));
  const home = path.join(dir, 'home');
  // Without it Home sends the window to the Setup screen, which is not what is under test.
  fs.mkdirSync(path.join(home, '.claude', 'lumen'), { recursive: true });
  fs.writeFileSync(path.join(home, '.claude', 'lumen', '.setup_done'), '');
  const env = {
    ...process.env,
    HOME: home,
    XDG_DATA_HOME: path.join(dir, 'data'),
    XDG_CONFIG_HOME: path.join(dir, 'config'),
    XDG_CACHE_HOME: path.join(dir, 'cache'),
    LANG: 'en_US.UTF-8',
  };
  delete env.LUMEN_DB;
  delete env.LUMEN_SIMULATE_TRAY;
  // Where Tauri's app_data_dir puts the ledger, and app_log_dir the log, for io.speedata.lumen.
  const appData = path.join(env.XDG_DATA_HOME, 'io.speedata.lumen');
  fs.mkdirSync(appData, { recursive: true });
  return { env, db: path.join(appData, 'lumen.db'), logs: path.join(appData, 'logs') };
}

/**
 * The CI runner's own profile, where Windows puts the app's ledger (%APPDATA%) and log
 * (%LOCALAPPDATA%) whatever the environment says, with what an earlier test left removed.
 */
function runnerProfile() {
  // Whatever the previous test launched and its driver's tree kill missed: a daemon whose app
  // was closed first is no longer under the driver, and while it runs it holds 127.0.0.1:9999,
  // which the next app's daemon then cannot bind.
  for (const exe of ['Lumen.exe', 'lumen-daemon.exe']) spawnSync('taskkill', ['/IM', exe, '/T', '/F']);
  const appData = path.join(process.env.APPDATA, 'io.speedata.lumen');
  const logs = path.join(process.env.LOCALAPPDATA, 'io.speedata.lumen', 'logs');
  const db = path.join(appData, 'lumen.db');
  // Retried: the previous test's app and daemon release their handles as they exit.
  const gone = { recursive: true, force: true, maxRetries: 20, retryDelay: 250 };
  for (const f of [db, `${db}-wal`, `${db}-shm`, logs]) fs.rmSync(f, gone);
  fs.mkdirSync(appData, { recursive: true });
  const marker = path.join(os.homedir(), '.claude', 'lumen', '.setup_done');
  fs.mkdirSync(path.dirname(marker), { recursive: true });
  fs.writeFileSync(marker, '');
  const env = { ...process.env };
  delete env.LUMEN_DB;
  delete env.LUMEN_SIMULATE_TRAY;
  return { env, db, logs };
}

/** Read `file` through the real lumen-mcp's smart_read, as Claude Code does, into the ledger. */
function smartRead(sb, file) {
  const input = [
    {
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e', version: '0' } },
    },
    { jsonrpc: '2.0', method: 'notifications/initialized' },
    { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'smart_read', arguments: { path: file } } },
  ].map(r => JSON.stringify(r) + '\n').join('');
  const env = { ...sb.env, LUMEN_DB: sb.db, LUMEN_RANKED_TIME_BUDGET_MS: '60000' };
  delete env.LUMEN_RANKED_OUTLINE;
  const r = spawnSync(MCP, [], { input, env, encoding: 'utf8' });
  assert.equal(r.status, 0, `lumen-mcp exited ${r.status}: ${r.stderr}`);
  const reply = r.stdout.split('\n').filter(Boolean).map(l => JSON.parse(l)).find(m => m.id === 2);
  assert.ok(reply?.result && !reply.result.isError, `smart_read ${file}: ${r.stdout}`);
}

/** What the ledger holds, read with SQLite directly rather than through anything under test. */
function ledger(db) {
  const d = new DatabaseSync(db, { readOnly: true });
  try {
    const { n } = d.prepare(
      `SELECT COALESCE(SUM(saved_tokens), 0) AS n FROM read_events
       WHERE routed_via IN ('smart_read', 'recall_file', 'compress_logs')`,
    ).get();
    const files = d.prepare('SELECT path, COUNT(*) AS reads FROM read_events GROUP BY path').all();
    return { optimized: Number(n), files: files.map(f => ({ path: f.path, reads: Number(f.reads) })) };
  } finally {
    d.close();
  }
}

/** The app's log, for the test output: what it decided at startup is the evidence. */
function appLog(sb) {
  try {
    return fs.readdirSync(sb.logs).map(f => fs.readFileSync(path.join(sb.logs, f), 'utf8')).join('');
  } catch {
    return '(no log file)';
  }
}

/**
 * Launch the app in `sb` with `env` added, attach to its main window, run `fn`, and stop it.
 * The app inherits its environment from tauri-driver, so each launch gets a driver of its own.
 */
async function withApp(t, sb, env, fn) {
  const driver = await Driver.start({ ...sb.env, ...env });
  let session;
  try {
    session = await Session.launch(APP);
    await mainWindow(session);
    return await fn(session);
  } finally {
    if (session) await session.quit().catch(() => {});
    await driver.stop();
    for (const line of appLog(sb).split('\n').filter(l => /TRAY|FALLBACK|DEGRADED/.test(l))) {
      t.diagnostic(line);
    }
  }
}

/** Switch to the window loading "/", which is the main one; the panel loads "/panel". */
async function mainWindow(s) {
  const deadline = Date.now() + 20_000;
  for (;;) {
    for (const h of await s.handles()) {
      await s.switchTo(h);
      if ((await s.run('return location.pathname')) === '/') return;
    }
    if (Date.now() > deadline) throw new Error('no window has "/" loaded');
    await sleep(250);
  }
}

/** Whether the main window is shown, as GTK reports it and as the page sees it. */
const VISIBILITY = `
  return window.__TAURI_INTERNALS__.invoke('plugin:window|is_visible', { label: 'main' })
    .then(shown => ({ shown, page: document.visibilityState }));`;

/** The same, but null until the window is shown. */
const SHOWN = VISIBILITY.replace('shown => ({', 'shown => shown && ({');

const HEALTH = `return window.__TAURI_INTERNALS__.invoke('lumen_startup_health');`;

/**
 * True, with the test marked skipped and the reason given, when the machine could not build a
 * tray icon at all. What follows is then about a tray this run does not have.
 */
async function noTrayHere(t, s) {
  const h = await s.run(HEALTH);
  if (!/^build failed/.test(h.tray)) return false;
  t.skip(`this machine cannot host a tray icon: ${h.tray}`);
  return true;
}

const digits = s => (s ?? '').replace(/\D/g, '');

/** The text of the first element matching the selector, once it has some. */
const TEXT = `return document.querySelector(arguments[0])?.textContent.trim() || null;`;

// Each banner text against what is behind it, as WCAG computes contrast: the colours as the
// page resolved them, with every translucent background between the text and the page
// composited in. The theme is whatever the platform reports, and is returned with the ratios.
const CONTRAST = `
  const rgba = s => {
    const m = /rgba?\\(([^)]*)\\)/.exec(s);
    const p = m ? m[1].split(/[\\s,/]+/).filter(Boolean).map(Number) : [0, 0, 0, 0];
    return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1];
  };
  const over = (top, under) => top.slice(0, 3).map((c, i) => c * top[3] + under[i] * (1 - top[3]));
  const lum = c => {
    const [r, g, b] = c.map(v => (v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const behind = el => {
    const chain = [];
    for (let e = el; e; e = e.parentElement) chain.unshift(rgba(getComputedStyle(e).backgroundColor));
    return chain.reduce((acc, c) => over(c, acc), [255, 255, 255]);
  };
  const theme = matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  const texts = [...document.querySelectorAll(arguments[0])].map(el => {
    const bg = behind(el);
    const fg = over(rgba(getComputedStyle(el).color), bg);
    const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
    const name = el.className || el.parentElement.className + ' ' + el.tagName.toLowerCase();
    return { name, color: getComputedStyle(el).color, ratio: Math.round(((hi + 0.05) / (lo + 0.05)) * 100) / 100 };
  });
  return { theme, texts };`;

test('a tray that cannot be built leaves the window open, saying so legibly', async t => {
  const sb = sandbox();
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'err' }, async s => {
    const v = await s.until('the main window to be shown', SHOWN);
    t.diagnostic(`visibility: ${JSON.stringify(v)}`);

    const tray = await s.until('the reduced-function banner', TEXT, 15_000, '.degraded__tray');
    assert.match(tray, /build failed/);
    // The gauge is redrawn as soon as the page starts, and finds no tray. Until 1.6.0 that was
    // reported as the tray having "disappeared after startup", replacing the reason above.
    await sleep(2_000);
    assert.match(await s.run(TEXT, '.degraded__tray'), /build failed/);

    const c = await s.run(CONTRAST, '.degraded__head, .degraded__tray, .degraded__item, .degraded__hint, .degraded__hint strong');
    t.diagnostic(`theme ${c.theme}: ${JSON.stringify(c.texts)}`);
    assert.ok(c.texts.length >= 4, `banner texts found: ${JSON.stringify(c.texts)}`);
    // 3:1 is WCAG's floor for text of any size. As the banner was first written, the bold "Report a
    // fault" was #e6edf3 on the light theme's near-white, about 1.1:1.
    for (const x of c.texts) assert.ok(x.ratio >= 3, `${x.name} ${x.color} is ${x.ratio}:1 in the ${c.theme} theme`);
  });
  // Opened once, by the setup gate; the redraw opened it a second time.
  assert.equal(appLog(sb).match(/FALLBACK: revealed the main window/g)?.length, 1, appLog(sb));
});

test('the Optimizer and Hotspots screens show what the ledger holds', async t => {
  const sb = sandbox();
  smartRead(sb, SETUP_RS);
  smartRead(sb, SETUP_RS);
  smartRead(sb, LIB_RS);
  const want = ledger(sb.db);
  t.diagnostic(`ledger: ${JSON.stringify(want)}`);
  assert.ok(want.optimized > 0, 'smart_read recorded no saving to show');
  const setup = want.files.find(f => path.basename(f.path) === 'setup.rs');
  assert.equal(setup?.reads, 2, `ledger rows: ${JSON.stringify(want.files)}`);

  // Simulated so the window is shown and its tabs can be clicked; the screens are the subject.
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'err' }, async s => {
    await s.click('a.tab-nav__tab[href="/optimizer"]');
    const shown = await s.until(
      `"Lumen optimized" to read ${want.optimized}`,
      `const t = document.querySelector('.savings-card--caused .savings-card__num')?.textContent;
       return t && t.replace(/\\D/g, '') === arguments[0] ? t.trim() : null;`,
      15_000,
      String(want.optimized),
    );
    t.diagnostic(`Optimizer, Lumen optimized: "${shown}"`);

    await s.click('a.tab-nav__tab[href="/hotspots"]');
    const rows = await s.until(
      'the hotspot rows',
      `const r = [...document.querySelectorAll('.hs__row')].map(r => ({
         name: r.querySelector('.hs__name')?.textContent.trim(),
         title: r.querySelector('.hs__name')?.title,
         meta: r.querySelector('.hs__meta')?.textContent.replace(/\\s+/g, ' ').trim(),
       }));
       return r.length ? r : null;`,
    );
    t.diagnostic(`Hotspots rows: ${JSON.stringify(rows)}`);
    // setup.rs, twice, outweighs lib.rs once.
    assert.equal(rows[0].name, 'setup.rs');
    assert.equal(rows[0].title, setup.path);
    assert.match(rows[0].meta, /^2 reads · [\d,]+ lines · [\d.]+% of all context/);
    assert.equal(rows.length, want.files.length);
    const across = await s.run(TEXT, '.hs__stat-sub');
    assert.equal(across, `across ${want.files.length} files`);
  });
});

test('an empty ledger shows the empty states rather than zeros dressed as data', async t => {
  const sb = sandbox();
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'err' }, async s => {
    await s.click('a.tab-nav__tab[href="/optimizer"]');
    const empty = await s.until('the Optimizer empty state', TEXT, 15_000, '.hero__empty-text');
    assert.match(empty, /^No optimizer events recorded yet\./);
    assert.equal(digits(await s.run(TEXT, '.savings-card--caused .savings-card__num')), '0');

    await s.click('a.tab-nav__tab[href="/hotspots"]');
    const none = await s.until('the Hotspots empty state', TEXT, 15_000, '.hs__empty');
    assert.match(none, /^No reads recorded yet\./);
    assert.equal(await s.run('return document.querySelectorAll(".hs__row").length'), 0);
  });
});

test('a healthy launch keeps its window closed past the tray checks', async t => {
  const sb = sandbox();
  await withApp(t, sb, {}, async s => {
    if (await noTrayHere(t, s)) return;
    // Off macOS the app cannot say where a tray icon is, so the checks have nothing to find.
    // Read as an absence, that opened this window six seconds into every launch and marked it
    // degraded.
    const until = Date.now() + PRESENCE_CHECKS_MS + 3_000;
    const seen = [];
    while (Date.now() < until) {
      const v = await s.run(VISIBILITY);
      seen.push(v);
      assert.equal(v.shown, false, `the window was shown ${Math.round((until - Date.now()) / 1000)} s before the end`);
      await sleep(500);
    }
    t.diagnostic(`${seen.length} samples, all hidden; page saw ${[...new Set(seen.map(v => v.page))]}`);
    const h = await s.run(HEALTH);
    t.diagnostic(`startup health: ${JSON.stringify(h)}`);
    assert.equal(h.degraded, false);
  });
});

test('a tray reported absent opens the window once the checks give up', async t => {
  const sb = sandbox();
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'absent' }, async s => {
    const start = Date.now();
    if (await noTrayHere(t, s)) return;
    assert.equal((await s.run(VISIBILITY)).shown, false, 'shown before any check had run');
    const v = await s.until('the main window to be shown', SHOWN, PRESENCE_CHECKS_MS + 10_000);
    t.diagnostic(`shown after ${Date.now() - start} ms: ${JSON.stringify(v)}`);
    const tray = await s.until('the reduced-function banner', TEXT, 15_000, '.degraded__tray');
    assert.match(tray, /built but not visible/);
  });
});
