// The relay-side throttle on the pair-code pass-through (#1086). With GitHub sign-in off for an install,
// the relay forwards two kinds of request it cannot judge itself: a pairing invite (`/?pair=…`) and a
// request carrying a `clk_`/`cph_` credential. The mothership decides both. What the relay does is make
// guessing pointless over the internet: every invite open counts, and every forwarded credential the
// mothership rejected counts, per install and per client, in fixed windows kept in D1 (the `throttle`
// table, migrations/0002_pair_code_access.sql). Past a limit the relay answers 429 itself and nothing
// is forwarded until the window ends.
//
// A client is the connecting IP, but never stored as one: its key is an HMAC under SESSION_SECRET,
// truncated, so the table holds no address and the key is useless outside this relay.

import { hmacSign } from './crypto.js';

/** Fixed-window limits: `limit` events per `seconds`, per client and per install. Invites are rare by
 * nature (the mothership keeps at most four open at once, each single-use), so a client gets a handful
 * of opens. Failures are what a guesser produces; a real browser with a stale cookie makes one or two,
 * because the relay clears that cookie on the first rejection. The per-install ceilings are generous:
 * they are there against a spread-out flood, and while one holds, a bound owner's GitHub session still
 * gets through (it never touches this table). */
export const LIMITS = {
  invite: { client: { limit: 10, seconds: 600 }, install: { limit: 30, seconds: 600 } },
  fail: { client: { limit: 20, seconds: 600 }, install: { limit: 200, seconds: 600 } },
};

/** The throttle key of the connecting client: an HMAC of its IP, never the IP itself. */
export async function clientKey(request, env) {
  const ip = request.headers.get('cf-connecting-ip') ?? 'unknown';
  return (await hmacSign(env.SESSION_SECRET, `client:${ip}`)).slice(0, 22);
}

const keysFor = (kind, installId, client) => [
  { key: `${kind}:install:${installId}`, limit: LIMITS[kind].install.limit, seconds: LIMITS[kind].install.seconds },
  { key: `${kind}:client:${client}`, limit: LIMITS[kind].client.limit, seconds: LIMITS[kind].client.seconds },
];

/** Whether any of `kinds` is over its limit for this install or this client right now. One read. */
export async function throttled(env, kinds, installId, client, now) {
  const rows = kinds.flatMap((kind) => keysFor(kind, installId, client));
  const { results } = await env.DB.prepare(
    `SELECT key, count FROM throttle WHERE window_ends > ? AND key IN (${rows.map(() => '?').join(', ')})`,
  )
    .bind(now, ...rows.map((row) => row.key))
    .all();
  const counts = new Map((results ?? []).map((row) => [row.key, row.count]));
  return rows.some((row) => (counts.get(row.key) ?? 0) >= row.limit);
}

/** Counts one event of `kind` against this install and this client: a fresh window for a key whose
 * window has ended, one more otherwise. Dead rows of any key are deleted in the same batch. */
export async function count(env, kind, installId, client, now) {
  const upsert = (row) =>
    env.DB.prepare(
      `INSERT INTO throttle (key, count, window_ends) VALUES (?, 1, ?)
       ON CONFLICT(key) DO UPDATE SET
         count = CASE WHEN window_ends <= ? THEN 1 ELSE count + 1 END,
         window_ends = CASE WHEN window_ends <= ? THEN excluded.window_ends ELSE window_ends END`,
    ).bind(row.key, now + row.seconds, now, now);
  await env.DB.batch([
    env.DB.prepare('DELETE FROM throttle WHERE window_ends <= ?').bind(now),
    ...keysFor(kind, installId, client).map(upsert),
  ]);
}

/** Retire drops an install's own rows with it; the per-client rows simply run out. */
export const forgetInstall = (env, installId) =>
  env.DB.prepare('DELETE FROM throttle WHERE key = ? OR key = ?').bind(`invite:install:${installId}`, `fail:install:${installId}`);
