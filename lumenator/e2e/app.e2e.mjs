// The built app, launched through WebDriver, showing a ledger the real lumen-mcp wrote.
//
// What the page specs cannot reach: the binary's own startup, whether it opens its window,
// and the Optimizer and Why screens rendering figures that came out of SQLite through
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
// takes those directories from the user's profile whatever the environment says (its known
// folders, not APPDATA or HOME), so no temporary directory can stand in for them: there the tests
// run only on a GitHub-hosted runner (GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted), a
// virtual machine discarded after the job, and use its profile, emptied before each test. Anywhere
// else, a self-hosted runner included, they refuse before touching anything.

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
import { writeFixtureLedger } from './fixture-ledger.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const EXE = process.platform === 'win32' ? '.exe' : '';
const APP = process.env.LUMEN_E2E_APP ?? path.join(ROOT, 'target', 'debug', `Lumen${EXE}`);
const MCP = process.env.LUMEN_E2E_MCP ?? path.join(ROOT, 'target', 'release', `lumen-mcp${EXE}`);
const CLI = process.env.LUMEN_E2E_CLI ?? path.join(ROOT, 'target', 'release', `lumen${EXE}`);
// Where the screenshots go; CI uploads this directory.
const ARTIFACTS = process.env.LUMEN_E2E_ARTIFACTS ?? path.join(os.tmpdir(), 'lumen-e2e-artifacts');
const SETUP_RS = path.join(ROOT, 'lumenator', 'src-tauri', 'src', 'setup.rs');
const LIB_RS = path.join(ROOT, 'lumenator', 'src-tauri', 'src', 'lib.rs');
// The identifiers come from where they are defined, so nothing here holds one as a literal:
// the app's from tauri.conf.json, and the one it replaced from the migration module.
const APP_ID = JSON.parse(
  fs.readFileSync(path.join(ROOT, 'lumenator', 'src-tauri', 'tauri.conf.json'), 'utf8'),
).identifier;
const LEGACY_APP_ID = /LEGACY_APP_ID: &str = "([^"]+)"/.exec(
  fs.readFileSync(path.join(ROOT, 'crates', 'lumen-core', 'src', 'legacy.rs'), 'utf8'),
)[1];

// How long `verify_tray_presence` in lib.rs takes to give up on the tray: 500 + 1,500 + 4,000 ms
// after RunEvent::Ready.
const PRESENCE_CHECKS_MS = 6_000;

const WINDOWS = process.platform === 'win32';

// The ranked outline's own time budget is 60 s (LUMEN_RANKED_TIME_BUDGET_MS in smartRead).
const MCP_MS = 120_000;

/** A GitHub-hosted runner: a machine nobody uses, thrown away after the job. */
const GITHUB_HOSTED = process.env.GITHUB_ACTIONS === 'true' && process.env.RUNNER_ENVIRONMENT === 'github-hosted';

if (!(process.platform === 'linux' || (WINDOWS && GITHUB_HOSTED))) {
  // Elsewhere the app's data and home directories come from the OS rather than from the
  // environment, so a run could not be kept off the real ledger.
  throw new Error('these tests run on Linux, and on Windows only on a GitHub-hosted runner; see the header');
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
  // Where Tauri's app_data_dir puts the ledger, and app_log_dir the log, for the app's identifier.
  const appData = path.join(env.XDG_DATA_HOME, APP_ID);
  fs.mkdirSync(appData, { recursive: true });
  return { env, db: path.join(appData, 'lumen.db'), logs: path.join(appData, 'logs') };
}

/**
 * The CI runner's own profile, where Windows puts the app's ledger (%APPDATA%) and log
 * (%LOCALAPPDATA%) whatever the environment says, with what an earlier test left removed.
 */
function runnerProfile() {
  // Checked again where it deletes, so no path to this function can empty a profile someone uses.
  if (!GITHUB_HOSTED) throw new Error('refusing to empty a Windows profile off a GitHub-hosted runner');
  const appDataRoot = process.env.APPDATA ?? '';
  if (!path.resolve(appDataRoot).toLowerCase().startsWith(path.resolve(os.homedir()).toLowerCase() + path.sep)) {
    throw new Error(`refusing: APPDATA ${appDataRoot} is not inside this runner's profile ${os.homedir()}`);
  }
  // Whatever the previous test launched and its driver's tree kill missed: a daemon whose app
  // was closed first is no longer under the driver, and while it runs it holds 127.0.0.1:9999,
  // which the next app's daemon then cannot bind.
  for (const exe of ['Lumen.exe', 'lumen-daemon.exe']) {
    spawnSync('taskkill', ['/IM', exe, '/T', '/F'], { timeout: 30_000 });
  }
  const appData = path.join(process.env.APPDATA, APP_ID);
  const logs = path.join(process.env.LOCALAPPDATA, APP_ID, 'logs');
  const db = path.join(appData, 'lumen.db');
  // Retried: the previous test's app and daemon release their handles as they exit.
  const gone = { recursive: true, force: true, maxRetries: 20, retryDelay: 250 };
  // The settings file too: a billing choice one test made would keep the next one's question away.
  const settings = path.join(appData, 'lumen_settings.json');
  for (const f of [db, `${db}-wal`, `${db}-shm`, logs, settings]) fs.rmSync(f, gone);
  // A directory of the one the app replaced would be migrated at start, which is not under test here.
  for (const d of [path.join(process.env.APPDATA, LEGACY_APP_ID), path.join(process.env.LOCALAPPDATA, LEGACY_APP_ID)]) {
    fs.rmSync(d, gone);
  }
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
  // Bounded: lumen-mcp leaves when its stdin closes, and a run that never saw it do so would
  // otherwise hold the job until CI's own limit.
  const r = spawnSync(MCP, [], { input, env, encoding: 'utf8', timeout: MCP_MS, killSignal: 'SIGKILL' });
  assert.ok(!r.error, `lumen-mcp did not finish smart_read ${file} within ${MCP_MS / 1000} s: ${r.error}\n${r.stderr}`);
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
    session = await Session.launch(driver, APP);
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

/**
 * The version the app was built as, from the two files that declare it: tauri.conf.json, which
 * Tauri compiles into the app and answers `getVersion()` from, and the Tauri crate's Cargo.toml.
 * Read when the test runs, so a release changes nothing here. Two files that disagree stop the
 * test before it looks at the window, which would otherwise have to pick one to be right.
 */
function builtVersion() {
  const dir = path.join(ROOT, 'lumenator', 'src-tauri');
  const conf = JSON.parse(fs.readFileSync(path.join(dir, 'tauri.conf.json'), 'utf8')).version;
  // The first version line is [package]'s; every dependency table comes after it.
  const cargo = /^version = "([^"]+)"/m.exec(fs.readFileSync(path.join(dir, 'Cargo.toml'), 'utf8'))?.[1];
  assert.ok(conf && cargo, `no version found: tauri.conf.json ${conf}, Cargo.toml ${cargo}`);
  assert.equal(conf, cargo, 'tauri.conf.json and the Tauri crate\'s Cargo.toml declare different versions');
  return conf;
}

/** Whether the first element matching the selector is laid out: present, with an area. */
const LAID_OUT = `
  const r = document.querySelector(arguments[0])?.getBoundingClientRect();
  return !!r && r.width > 0 && r.height > 0;`;

// How the About section, on Settings since 1.9.0, is built: a region named by its heading, the
// version as the value of a "Version" term, and the version actually laid out.
const ABOUT = `
  const section = document.querySelector('section.about');
  const heading = document.getElementById(section?.getAttribute('aria-labelledby') ?? '');
  const r = document.querySelector('section.about .about__version')?.getBoundingClientRect();
  return {
    heading: heading?.textContent.trim() ?? null,
    term: section?.querySelector('dl > dt')?.textContent.trim() ?? null,
    valueFollowsTerm: section?.querySelector('dl > dt + dd')?.classList.contains('about__version') ?? false,
    aboutLaidOut: !!r && r.width > 0 && r.height > 0,
  };`;

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

test('the Optimizer and Why screens show what the ledger holds', async t => {
  const sb = sandbox();
  // Ten reads of an unchanged file is what the Why screen exists to point at (at least ten
  // reads, half of them unchanged), and what puts it in the window's tabs.
  for (let i = 0; i < 10; i++) smartRead(sb, SETUP_RS);
  smartRead(sb, LIB_RS);
  const want = ledger(sb.db);
  t.diagnostic(`ledger: ${JSON.stringify(want)}`);
  assert.ok(want.optimized > 0, 'smart_read recorded no saving to show');
  const setup = want.files.find(f => path.basename(f.path) === 'setup.rs');
  assert.equal(setup?.reads, 10, `ledger rows: ${JSON.stringify(want.files)}`);

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

    await s.click('a.tab-nav__tab[href="/why"]');
    const todo = await s.until(
      'what to do',
      `const r = [...document.querySelectorAll('.why__item .why__name')].map(n => n.textContent.trim());
       return r.length ? r : null;`,
    );
    t.diagnostic(`Why, what to do: ${JSON.stringify(todo)}`);
    assert.deepEqual(todo, ['setup.rs']);
    const rows = await s.until(
      'the hotspot rows',
      `const r = [...document.querySelectorAll('.hs__row')].map(r => ({
         name: r.querySelector('.hs__name')?.textContent.trim(),
         title: r.querySelector('.hs__name')?.title,
         meta: r.querySelector('.hs__meta')?.textContent.replace(/\\s+/g, ' ').trim(),
       }));
       return r.length ? r : null;`,
    );
    t.diagnostic(`Why rows: ${JSON.stringify(rows)}`);
    // setup.rs, ten times, outweighs lib.rs once.
    assert.equal(rows[0].name, 'setup.rs');
    assert.equal(rows[0].title, setup.path);
    assert.match(rows[0].meta, /^10 reads · [\d,]+ lines · [\d.]+% of all context · 90% unchanged/);
    assert.equal(rows.length, want.files.length);
    const across = await s.run(TEXT, '.hs__stat-sub');
    assert.equal(across, `across ${want.files.length} files`);
  });
});

test('an empty ledger shows the empty states rather than zeros dressed as data', async t => {
  const sb = sandbox();
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'err' }, async s => {
    // Home: the billing question, since this sandbox's settings file has no answer, and a
    // headline that says nothing was recorded, in words, with no figure.
    const asked = await s.until('the billing question', TEXT, 15_000, 'lumen-billing-prompt .prompt__head');
    assert.equal(asked, 'How do you pay for Claude Code?');
    const none = await s.until('the headline to say nothing was recorded', TEXT, 15_000, 'lumen-savings-headline .headline__none');
    assert.equal(none, 'No Lumen activity recorded yet.');
    assert.equal(await s.run('return document.querySelectorAll("lumen-savings-headline strong").length'), 0);
    assert.equal(await s.run(TEXT, '.status__text'), 'Status: no data');

    await s.click('a.tab-nav__tab[href="/optimizer"]');
    const empty = await s.until('the Optimizer empty state', TEXT, 15_000, '.hero__empty-text');
    assert.match(empty, /^No optimizer events recorded yet\./);
    assert.equal(digits(await s.run(TEXT, '.savings-card--caused .savings-card__num')), '0');

    // Nothing read, so nothing to act on: Why is not offered at all.
    await s.until('the Optimizer screen\'s tabs', TEXT, 15_000, 'a.tab-nav__tab[href="/settings"]');
    assert.equal(await s.run('return document.querySelectorAll(\'a.tab-nav__tab[href="/why"]\').length'), 0);
  });
});

test('the window shows the version the app was built as, in its header and in About', async t => {
  const want = builtVersion();
  const sb = sandbox();
  // Simulated so the window is shown; the version is the subject. The page asks the Tauri runtime
  // for it (`getVersion()`), so what is read here went through the built app, not a test double.
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'err' }, async s => {
    const head = await s.until('the version in the header', TEXT, 15_000, '.home__version');
    assert.equal(head, `Lumen ${want}`);
    assert.equal(await s.run(LAID_OUT, '.home__head .home__version'), true);

    // About has been on Settings since 1.9.0.
    await s.click('a.tab-nav__tab[href="/settings"]');
    const about = await s.until('the version in About', TEXT, 15_000, '.about__version');
    t.diagnostic(`built as ${want}; header "${head}", About "${about}"`);
    assert.equal(about, want);

    const a = await s.run(ABOUT);
    assert.deepEqual(a, {
      heading: 'About',
      term: 'Version',
      valueFollowsTerm: true,
      aboutLaidOut: true,
    });
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
  let skipped = false;
  await withApp(t, sb, { LUMEN_SIMULATE_TRAY: 'absent' }, async s => {
    const start = Date.now();
    skipped = await noTrayHere(t, s);
    if (skipped) return;
    // How long after launch this attaches is not the test's to set: on a CI runner the page's first
    // load has taken from 0.1 s to 16 s, and the window opens PRESENCE_CHECKS_MS after launch. So a
    // sample of the window cannot say "before the checks ran" by when it is taken. What it can say
    // is what the app says about itself a moment later: it records the tray as absent, which
    // is what degraded reports, before it opens the window.
    const shown = (await s.run(VISIBILITY)).shown;
    const h = await s.run(HEALTH);
    assert.ok(!shown || h.degraded, `shown while the checks had not given up: ${JSON.stringify(h)}`);
    const v = await s.until('the main window to be shown', SHOWN, PRESENCE_CHECKS_MS + 10_000);
    t.diagnostic(`shown after ${Date.now() - start} ms: ${JSON.stringify(v)}`);
    const tray = await s.until('the reduced-function banner', TEXT, 15_000, '.degraded__tray');
    assert.match(tray, /built but not visible/);
  });
  if (skipped) return;
  // The log has each step in the order it happened, whenever the test attached: the window opens
  // once, after the third check, and not before.
  const log = appLog(sb);
  const steps = [
    'TRAY: presence check 1 of 3',
    'TRAY: presence check 2 of 3',
    'TRAY: presence check 3 of 3',
    'TRAY: not visible after 3 checks',
    'FALLBACK: revealed the main window',
  ];
  const at = steps.map(s => log.indexOf(s));
  assert.ok(at.every((n, i) => n >= 0 && (i === 0 || n > at[i - 1])), `steps at ${at}, expected in order:\n${log}`);
  assert.equal(log.match(/FALLBACK: revealed the main window/g).length, 1, log);
});

// Last: on Windows the settings file lives in the runner's profile, and the billing choice this
// writes would otherwise reach the tests after it (runnerProfile removes it too).
test('Home shows in bold what the ledger saved, as `lumen` says it, and keeps a picture of it', async t => {
  const sb = sandbox();
  const wrote = writeFixtureLedger(sb.db, { mcp: MCP, env: sb.env, billing: 'subscription' });
  t.diagnostic(`fixture: ${JSON.stringify(wrote)}`);
  // The terminal's figures from the same ledger, through the built CLI.
  const lumen = (...args) => {
    const r = spawnSync(CLI, args, { env: { ...sb.env, LUMEN_DB: sb.db }, encoding: 'utf8', timeout: 60_000 });
    assert.equal(r.status, 0, `lumen ${args.join(' ')}: ${r.error ?? r.stderr}`);
    return r.stdout.trim();
  };
  const line = lumen();
  const summary = JSON.parse(lumen('report', '--json'));
  t.diagnostic(`lumen: ${line}`);
  assert.ok(summary.week.measured && summary.allTime.measured, 'premise: the fixture has dollars on record in both periods');

  await withApp(t, sb, {}, async s => {
    // A healthy launch keeps its window closed: open it, as the menu's Open Lumen does.
    await s.run(`return window.__TAURI_INTERNALS__.invoke('plugin:window|show', { label: 'main' })`);
    const boxes = await s.until('the headline\'s figures', `
      const t = (p, sel) => p?.querySelector(sel)?.textContent.trim() ?? null;
      const b = [...document.querySelectorAll('lumen-savings-headline .headline__period')].map(p => {
        const [tokens, usd] = p.querySelectorAll('.headline__figure');
        return {
          share: t(p, '.headline__share'),
          tokens: t(tokens, 'strong'),
          tokensUnit: t(tokens, '.headline__unit'),
          usdLead: t(usd, '.headline__floor'),
          usd: t(usd, 'strong'),
          usdTail: t(usd, '.headline__unit'),
          basis: t(p, '.headline__basis'),
          beside: [...p.querySelectorAll('.headline__tag')].map(x => x.textContent.trim()),
        };
      });
      return b.length === 2 && b.every(x => x.tokens && x.usd) ? b : null;`, 30_000);
    t.diagnostic(`Home: ${JSON.stringify(boxes)}`);
    const [week, all] = boxes;
    // Bold, both of them, in every period: tokens and dollars.
    const bold = await s.run(`return [...document.querySelectorAll('lumen-savings-headline strong')]
      .map(e => Number(getComputedStyle(e).fontWeight))`);
    assert.ok(bold.length === 4 && bold.every(w => w >= 700), `font weights: ${bold}`);
    // The same figures, signs and labels as the terminal's line, from the same ledger: each box,
    // piece by piece, is that period's part of the line.
    const body = (b, when) => [
      b.share.slice(0, -(when.length + 1)),
      `${b.tokens} ${b.tokensUnit}`,
      [b.usdLead, b.usd, b.usdTail, b.basis].filter(Boolean).join(' '),
    ].join('; ') + (b.beside.length ? ` (${b.beside.join('; ')})` : '');
    const said = `Lumen this week: ${body(week, 'this week')} · all time: ${body(all, 'all time')} · `;
    assert.ok(line.startsWith(said), `${said}\n${line}`);
    assert.equal(week.basis, 'at API prices');
    assert.ok(all.beside.some(b => b.endsWith(' modelled')) && all.beside.includes('1 call unpriced'), JSON.stringify(all));
    const status = await s.run(TEXT, '.status__text');
    assert.equal(status, 'Status: working');
    assert.ok(line.includes(' · working ('), line);

    // The picture for the release: Home as it renders on this platform.
    await sleep(500);
    fs.mkdirSync(ARTIFACTS, { recursive: true });
    const png = path.join(ARTIFACTS, `home-${WINDOWS ? 'windows' : 'linux'}.png`);
    fs.writeFileSync(png, await s.screenshot());
    t.diagnostic(`screenshot: ${png} (${fs.statSync(png).size} bytes)`);
    assert.ok(fs.statSync(png).size > 1_000, 'the screenshot holds a picture');

    // How it's measured, from the headline: the per-call ledger, a row per line the summary holds.
    await s.click('a.headline__how');
    const rows = await s.until('the per-call ledger',
      `return document.querySelectorAll('table[aria-labelledby="ledger-recent"] tbody tr').length || null;`);
    assert.equal(rows, summary.ledger.recent.length);
  });
});
