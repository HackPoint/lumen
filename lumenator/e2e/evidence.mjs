// A release's evidence from the app as it was installed: drive the INSTALLED app through
// WebDriver, open its main window, and keep a screenshot of every tab and what the screens say.
// Windows and Linux (tauri-driver); scripts/evidence/macos.sh does macOS through the
// accessibility API. It launches the installed app on the runner's real profile, so it runs only
// on a GitHub-hosted runner.
//
// usage: node e2e/evidence.mjs <installed app> <out dir> <label>
// writes: home-<label>.png, optimizer-<label>.png, details-<label>.png, settings-<label>.png,
//         app-<label>.json (the headline, the version on Settings, the Repeat reads switch)

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

const TEXT = `return document.querySelector(arguments[0])?.textContent.replace(/\\s+/g, ' ').trim() || null;`;

/** Switch to the window loading "/", the main one; the panel loads "/panel". */
async function mainWindow(s) {
  const deadline = Date.now() + 30_000;
  for (;;) {
    for (const h of await s.handles()) {
      await s.switchTo(h);
      if ((await s.run('return location.pathname')) === '/') return;
    }
    if (Date.now() > deadline) throw new Error('no window has "/" loaded');
    await sleep(250);
  }
}

async function shoot(s, name) {
  const file = path.join(out, `${name}-${label}.png`);
  fs.writeFileSync(file, await s.screenshot());
  const size = fs.statSync(file).size;
  if (size <= 1_000) throw new Error(`${file} holds no picture (${size} bytes)`);
  console.log(`screenshot: ${file} (${size} bytes)`);
}

const said = { label, app };
const driver = await Driver.start({ ...process.env });
let s;
try {
  s = await Session.launch(driver, app);
  await mainWindow(s);
  // A healthy launch keeps the window closed: open it, as the tray menu's Open Lumen does.
  await s.run(`return window.__TAURI_INTERNALS__.invoke('plugin:window|show', { label: 'main' })`);
  // Home: the savings headline, or what it says when nothing is recorded yet.
  said.home = await s.until('the savings headline', TEXT, 30_000, 'lumen-savings-headline');
  await sleep(1_500);
  await shoot(s, 'home');
  for (const tab of ['optimizer', 'details', 'settings']) {
    await s.click(`a.tab-nav__tab[href="/${tab}"]`);
    await s.until(`the ${tab} tab`,
      `return document.querySelector('a.tab-nav__tab[href="/' + arguments[0] + '"]')?.classList.contains('is-active') || null;`,
      15_000, tab);
    await sleep(1_000);
    await shoot(s, tab);
  }
  said.version = await s.run(TEXT, 'section.about .about__version');
  said.repeatReads = await s.run(`
    const sec = document.querySelector('section.repeat');
    const sw = sec?.querySelector('input[type=checkbox], [role=switch]');
    if (!sw) return null;
    return sw.checked ?? sw.getAttribute('aria-checked');`);
} finally {
  if (s) await s.quit().catch(e => console.error(`quit: ${e}`));
  await driver.stop();
}
fs.writeFileSync(path.join(out, `app-${label}.json`), JSON.stringify(said, null, 2));
console.log(JSON.stringify(said));
