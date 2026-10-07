// The worker driven end to end with a fake D1 and a bare ctx, so no Workers runtime is needed. The prune
// state (lastPrune in src/worker.js) is module-level, so the tests here lean on that honestly: the first
// request in this file always prunes, and the throttle assertions ride in the same ordered run rather than
// reaching into the module; the last test advances the clock past the throttle window instead.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { NEXT_IN_SECONDS, ONLINE_SECONDS, PRUNE_EVERY_MS, RETAIN_SECONDS } from '../src/presence.js';
import worker from '../src/worker.js';

const ID = '0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b';

// A D1 stand-in that records every statement with its bindings, and answers like D1 does.
function fakeDb() {
  const queries = [];
  return {
    queries,
    prepare(sql) {
      const query = { sql, bindings: null };
      queries.push(query);
      return {
        bind(...bindings) {
          query.bindings = bindings;
          return {
            run: async () => ({ success: true }),
            all: async () => ({ results: [] }),
          };
        },
      };
    },
  };
}

function fakeEnv(db) {
  return {
    DB: db,
    LIMITER: {
      async limit() {
        return { success: true };
      },
    },
  };
}

// Runs a request through the worker, then waits for whatever it handed to waitUntil (the prune).
async function serve(request, env) {
  const waits = [];
  const response = await worker.fetch(request, env, { waitUntil: (promise) => waits.push(promise) });
  await Promise.all(waits);
  return response;
}

const presenceRequest = () => new Request('https://telemetry.colonizer.dev/v1/presence', { method: 'GET' });
const heartbeatRequest = () =>
  new Request('https://telemetry.colonizer.dev/v1/heartbeat', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'cf-connecting-ip': '198.51.100.7' },
    body: JSON.stringify({ install_id: ID, version: '0.1.3', platform: 'darwin-arm64', colonies: 2 }),
  });

const prunes = (db) => db.queries.filter((q) => q.sql.startsWith('DELETE FROM presence WHERE seen_at <'));
const about = (value, expected, label) =>
  assert.ok(Math.abs(value - expected) <= 2, `${label} bound ${value}, expected about ${expected}`);

test('a presence request prunes stale rows and reads only the online window', async () => {
  const db = fakeDb();
  const response = await serve(presenceRequest(), fakeEnv(db));
  assert.equal(response.status, 200);

  // The regression this issue is about: serving the map also deletes rows past the retention window...
  const deletes = prunes(db);
  assert.equal(deletes.length, 1);
  assert.match(deletes[0].sql, /^DELETE FROM presence WHERE seen_at < \?$/);
  about(deletes[0].bindings[0], Math.floor(Date.now() / 1000) - RETAIN_SECONDS, 'the prune');

  // ...and the read itself never counts anything past the online window, deleted or not.
  const select = db.queries.find((q) => q.sql.includes('WHERE seen_at >= ?'));
  assert.ok(select, 'the presence query should filter on seen_at');
  about(select.bindings[0], Math.floor(Date.now() / 1000) - ONLINE_SECONDS, 'the online cutoff');

  const view = await response.json();
  assert.equal(view.motherships, 0);
  assert.equal(view.online_window_seconds, ONLINE_SECONDS);
});

test('requests straight after do not prune again, whichever route they take', async () => {
  // Same route, straight after: throttled, but the map still answers.
  const presenceDb = fakeDb();
  const response = await serve(presenceRequest(), fakeEnv(presenceDb));
  assert.equal(response.status, 200);
  assert.equal(prunes(presenceDb).length, 0);

  // The other route too: the throttle is shared, and the heartbeat itself is still stored.
  const heartbeatDb = fakeDb();
  const beat = await serve(heartbeatRequest(), fakeEnv(heartbeatDb));
  assert.equal(beat.status, 200);
  assert.deepEqual(await beat.json(), { ok: true, next_in: NEXT_IN_SECONDS });
  assert.equal(prunes(heartbeatDb).length, 0);
  assert.ok(heartbeatDb.queries.some((q) => q.sql.startsWith('INSERT INTO presence')));
});

test('a heartbeat prunes too, once the throttle window has passed', async (t) => {
  // The 10 minute window is far too long to wait out, so advance the clock instead. Twice the window from
  // the real now, so the test does not care exactly when the last prune happened.
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
  t.mock.timers.tick(2 * PRUNE_EVERY_MS);

  const db = fakeDb();
  const response = await serve(heartbeatRequest(), fakeEnv(db));
  assert.equal(response.status, 200);

  const deletes = prunes(db);
  assert.equal(deletes.length, 1);
  assert.match(deletes[0].sql, /^DELETE FROM presence WHERE seen_at < \?$/);
  about(deletes[0].bindings[0], Math.floor(Date.now() / 1000) - RETAIN_SECONDS, 'the prune');
});
