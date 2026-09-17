// Colonizer's telemetry service: the live map on colonizer.dev. Motherships whose user switched the live map
// on send a heartbeat every 5 minutes; the map shows how many are online, where to within about 25 km, and
// how many colonies they are running. docs/telemetry.md is the full account.
//
//   POST /v1/heartbeat   a mothership's heartbeat (or {"online": false} when it is switched off)
//   GET  /v1/presence    the public view: totals, and counts per 25 km cell
//
// The location comes from Cloudflare's own estimate for the request (request.cf), is snapped to a grid cell
// here, and only the cell is stored. The IP address is used for rate limiting in memory and never stored.

import { NEXT_IN_SECONDS, ONLINE_SECONDS, RETAIN_SECONDS, cellOf, installKey, parseHeartbeat, presenceView } from './presence.js';

const MAX_BODY_BYTES = 1024;

// Nothing is kept for long: rows older than an hour are deleted, at most once every 10 minutes per isolate,
// on the back of a heartbeat. (A cron trigger would need a workers.dev subdomain on the account.)
const PRUNE_EVERY_MS = 10 * 60 * 1000;
let lastPrune = 0;

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (request.method === 'OPTIONS') return cors(new Response(null, { status: 204 }));
    if (url.pathname === '/v1/heartbeat' && request.method === 'POST') {
      if (Date.now() - lastPrune > PRUNE_EVERY_MS) {
        lastPrune = Date.now();
        ctx.waitUntil(prune(env));
      }
      return heartbeat(request, env);
    }
    if (url.pathname === '/v1/presence' && request.method === 'GET') return presence(env);
    if (url.pathname === '/' && request.method === 'GET') {
      return new Response(
        'Colonizer telemetry: the live map on https://colonizer.dev/live.\nWhat is collected, and how to switch it off: https://colonizer.dev/docs/telemetry\n',
        { headers: { 'content-type': 'text/plain; charset=utf-8' } },
      );
    }
    return json({ error: 'not found' }, 404);
  },
};

async function prune(env) {
  const now = Math.floor(Date.now() / 1000);
  await env.DB.prepare('DELETE FROM presence WHERE seen_at < ?').bind(now - RETAIN_SECONDS).run();
}

async function heartbeat(request, env) {
  const ip = request.headers.get('cf-connecting-ip') ?? 'unknown';
  if (env.LIMITER) {
    const { success } = await env.LIMITER.limit({ key: ip });
    if (!success) return json({ error: 'slow down' }, 429);
  }
  const text = await request.text();
  if (text.length > MAX_BODY_BYTES) return json({ error: 'body too large' }, 413);
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return json({ error: 'body must be JSON' }, 400);
  }
  const beat = parseHeartbeat(body);
  if (beat.error) return json({ error: beat.error }, 400);

  const key = await installKey(beat.installId);
  if (!beat.online) {
    await env.DB.prepare('DELETE FROM presence WHERE install = ?').bind(key).run();
    return json({ ok: true });
  }
  const cell = cellOf(request.cf?.latitude, request.cf?.longitude);
  const now = Math.floor(Date.now() / 1000);
  await env.DB.prepare(
    `INSERT INTO presence (install, cell_lat, cell_lon, colonies, version, platform, seen_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
     ON CONFLICT (install) DO UPDATE SET cell_lat = ?2, cell_lon = ?3, colonies = ?4, version = ?5, platform = ?6, seen_at = ?7`,
  )
    .bind(key, cell?.lat ?? null, cell?.lon ?? null, beat.colonies, beat.version, beat.platform, now)
    .run();
  return json({ ok: true, next_in: NEXT_IN_SECONDS });
}

async function presence(env) {
  const now = Math.floor(Date.now() / 1000);
  const { results } = await env.DB.prepare(
    `SELECT cell_lat, cell_lon, COUNT(*) AS motherships, SUM(colonies) AS colonies
     FROM presence WHERE seen_at >= ? GROUP BY cell_lat, cell_lon`,
  )
    .bind(now - ONLINE_SECONDS)
    .all();
  return cors(json(presenceView(results ?? [], now), 200, { 'cache-control': 'public, max-age=30' }));
}

function json(value, status = 200, headers = {}) {
  return new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json', ...headers } });
}

// The presence view is public data for the map on colonizer.dev; heartbeats come from motherships, not
// browsers, so they need no CORS at all.
function cors(response) {
  response.headers.set('access-control-allow-origin', '*');
  response.headers.set('access-control-allow-methods', 'GET');
  return response;
}
