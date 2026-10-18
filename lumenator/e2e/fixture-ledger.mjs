// A ledger with a known shape, for the headline e2e and the screenshots of Home.
//
// Written relative to the time it is written, so whenever it runs the app shows the same kinds
// of figure: this week, a conversation with two smart_read calls read by an Opus 4.8 turn and a
// dozen rounds after; last week, a Sonnet 4.6 conversation from before the daemon recorded
// compactions (so part of its worth is modelled, beside the measured figure); and a call with no
// session, which has tokens and no dollars, so the data is partial. What the figures come to is
// the engine's to say: the e2e reads them back through `lumen report --json` and holds the window
// to them, never to numbers written here.
//
// Used by lumenator/e2e/app.e2e.mjs, and from the command line by scripts/issue5-launch-check.sh:
//   node lumenator/e2e/fixture-ledger.mjs --db <lumen.db> --mcp <lumen-mcp> [--billing subscription|api]
// It only ever writes a ledger it creates: given a path where a ledger, or anything else, already
// is, it refuses and changes nothing, so it cannot touch a ledger someone uses.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

/** ISO-8601 UTC to the second, as the meter writes a call's time. */
const iso = ms => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');
/** ISO-8601 UTC to the millisecond, as a transcript writes a turn's. */
const isoMs = ms => new Date(ms).toISOString();

/** Monday 00:00 of the ISO week `now` is in, local time: where the engine's week begins. */
function weekStart(now) {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - ((d.getDay() + 6) % 7));
  return d.getTime();
}

/**
 * A ledger with the app's schema at `db`, made by the real lumen-mcp, which creates and migrates
 * it as Claude Code's calls do. The one row its call wrote is removed by the caller.
 */
function createLedger(db, mcp, env) {
  const input = [
    {
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'fixture', version: '0' } },
    },
    { jsonrpc: '2.0', method: 'notifications/initialized' },
    {
      jsonrpc: '2.0', id: 2, method: 'tools/call',
      params: { name: 'smart_read', arguments: { path: fileURLToPath(import.meta.url) } },
    },
  ].map(r => JSON.stringify(r) + '\n').join('');
  const r = spawnSync(mcp, [], {
    input,
    env: { ...env, LUMEN_DB: db },
    encoding: 'utf8',
    timeout: 120_000,
    killSignal: 'SIGKILL',
  });
  if (r.error || r.status !== 0 || !fs.existsSync(db)) {
    throw new Error(`lumen-mcp did not create ${db}: ${r.error ?? `exit ${r.status}`}\n${r.stderr}`);
  }
}

/**
 * Write the fixture into `db`, creating the ledger with `mcp` if there is none, and the billing
 * choice beside it. Returns what it wrote, for the test's diagnostics.
 */
export function writeFixtureLedger(db, { mcp, env = process.env, now = Date.now(), billing = 'subscription' } = {}) {
  const settings = path.join(path.dirname(db), 'lumen_settings.json');
  for (const f of [db, `${db}-wal`, `${db}-shm`, settings]) {
    if (fs.existsSync(f)) throw new Error(`refusing to write a fixture over ${f}, which is already there`);
  }
  fs.mkdirSync(path.dirname(db), { recursive: true });
  createLedger(db, mcp, env);

  const monday = weekStart(now);
  // Six hours ago, so the conversation has ended (the engine counts one under an hour old as
  // possibly still running), unless the week is younger than that.
  const week = Math.max(monday + 60_000, now - 6 * HOUR);
  const last = monday - 3 * DAY;

  const d = new DatabaseSync(db);
  try {
    d.exec('BEGIN');
    // The one row lumen-mcp wrote making this ledger a moment ago, and nothing else: a ledger
    // holding more is not the one it made.
    const { reads } = d.prepare('SELECT COUNT(*) AS reads FROM read_events').get();
    if (Number(reads) !== 1) throw new Error(`${db} holds ${reads} reads, not the one lumen-mcp just wrote`);
    d.exec('DELETE FROM read_events');

    let n = 0;
    const turn = d.prepare(
      `INSERT INTO turns(message_id, session_id, ts, model, input_tokens, output_tokens,
         cache_read_input_tokens, cache_creation_input_tokens, cache_5m, cache_1h, source_file,
         is_subagent, speed, inference_geo)
       VALUES(?, ?, ?, ?, 12, ?, ?, 2000, 2000, 0, ?, 0, 'standard', 'global')`,
    );
    const call = d.prepare(
      `INSERT INTO read_events(ts, tool, path, lines, tokens_returned, full_tokens, saved_tokens,
         routed_via, channel, session_id, token_source, repeat_arm, repeat_plan)
       VALUES(?, ?, ?, 600, ?, ?, ?, 'smart_read', 'cli', ?, 'measured', ?, ?)`,
    );
    /** A conversation of `rounds` turns after its first, on `model`, from `start`. */
    const conversation = (session, start, model, rounds) => {
      for (let k = 0; k <= rounds; k++) {
        n++;
        turn.run(`msg_fixture_${n}`, session, isoMs(start + k * 20_000), model, 300 + k * 40,
          30_000 + k * 2_500, `/fixture/.claude/projects/-repo/${session}.jsonl`);
      }
    };
    const smartRead = (session, ms, file, full, returned) => call.run(
      iso(ms), 'mcp__lumen__smart_read', file, returned, full, full - returned, session,
      session ? 'treatment' : null, session ? 'first' : null,
    );

    // The daemon began recording a day before this week: last week's conversation is older.
    d.prepare(`INSERT INTO session_events(session_id, kind, uuid, ts_ms, source_file)
               VALUES('fixture-resync', 'resync', 'fixture-0', ?, '/fixture/.claude/projects/-repo/fixture-resync.jsonl')`)
      .run(monday - DAY);

    conversation('fixture-week', week, 'claude-opus-4-8', 12);
    smartRead('fixture-week', week + 2_000, '/repo/src/engine.rs', 18_000, 1_400);
    smartRead('fixture-week', week + 62_000, '/repo/src/parser.ts', 9_500, 700);

    conversation('fixture-last', last, 'claude-sonnet-4-6', 8);
    smartRead('fixture-last', last + 2_000, '/repo/README.md', 7_000, 600);

    // From before the meter recorded sessions: tokens, and no dollars.
    smartRead(null, last - DAY, '/repo/old/setup.py', 4_000, 700);
    d.exec('COMMIT');
  } catch (e) {
    d.exec('ROLLBACK');
    throw e;
  } finally {
    d.close();
  }

  // Where lumen_core::settings keeps the billing choice: beside the ledger.
  fs.writeFileSync(settings, JSON.stringify({ billing_mode: billing }));
  return { db, billing, week: iso(week), last: iso(last) };
}

// From the command line.
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const arg = name => {
    const i = process.argv.indexOf(`--${name}`);
    return i > 0 ? process.argv[i + 1] : undefined;
  };
  const db = arg('db');
  if (!db) {
    console.error('usage: fixture-ledger.mjs --db <lumen.db> --mcp <lumen-mcp> [--billing subscription|api]');
    process.exit(2);
  }
  console.log(JSON.stringify(writeFixtureLedger(db, { mcp: arg('mcp'), billing: arg('billing') ?? 'subscription' })));
}
