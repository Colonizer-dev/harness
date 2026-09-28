// The pure side of tunnel protocol v1: sizes, header stripping, and the one-way path template used in logs.

/** Concurrent streams per install (HTTP and WS passthrough both count). */
export const MAX_STREAMS = 32;

/** Concurrent pending (not yet verified) handshakes per install: past this, new dials are refused
 * 1013 until one settles. Handshakes are independent, so a re-dial loop cannot starve the real one. */
export const MAX_PENDING = 16;

/** Raw bytes per outbound chunk: base64 of this is exactly 48 KiB = 49152 chars, so either reading of
 * "chunks ≤ 48 KiB" holds. */
export const CHUNK_RAW = 36864;

/** Inbound chunks must decode to at most this many bytes. */
export const CHUNK_DECODED_MAX = 49152;

/** Seconds between tunnel pings. */
export const PING_MS = 20000;

/** Seconds of hello signature slack in either direction. */
export const TS_SKEW = 300;

const HOP_BY_HOP = new Set([
  'connection',
  'keep-alive',
  'proxy-authenticate',
  'proxy-authorization',
  'te',
  'trailer',
  'trailers',
  'transfer-encoding',
  'upgrade',
  'proxy-connection',
]);

// The handshake headers the mothership must not see or replay on a ws_open (the relay already accepted the
// browser's socket; sec-websocket-protocol survives so the cockpit's /ws can negotiate subprotocols).
const WS_HANDSHAKE = new Set(['sec-websocket-key', 'sec-websocket-version', 'sec-websocket-extensions', 'sec-websocket-accept']);

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const HEX = /^[0-9a-f]+$/i;
const TOKEN = /^[a-z0-9]+$/i;

/** One [name, value] entry per value: an array value means repeats and is flattened. */
function* eachValue(name, value) {
  if (Array.isArray(value)) for (const one of value) yield [name, one];
  else yield [name, value];
}

/** Flattens any accepted shape — Headers, [name, value] pairs, or a plain object — into single
 * [name, value] entries. Headers iteration can join repeated set-cookie values, so those come from
 * getSetCookie() instead when it exists. */
function* eachHeader(headers) {
  if (Array.isArray(headers)) {
    for (const [name, value] of headers) yield* eachValue(name, value);
  } else if (typeof headers?.entries === 'function') {
    for (const [name, value] of headers.entries()) {
      if (name !== 'set-cookie') yield* eachValue(name, value);
    }
    if (typeof headers.getSetCookie === 'function') {
      for (const cookie of headers.getSetCookie()) yield ['set-cookie', cookie];
    }
  } else {
    for (const [name, value] of Object.entries(headers ?? {})) yield* eachValue(name, value);
  }
}

/** Strips hop-by-hop headers, everything named in Connection, and any x-relay-* header; for a ws_open also
 * the WebSocket handshake headers. Accepts Headers, [name, value] pairs, or a plain object, and returns
 * lower-cased [name, value] pairs with repeats kept — the wire shape, so repeated set-cookie survives. */
export function stripHopByHop(headers, { ws = false } = {}) {
  const out = [];
  const connections = [];
  for (const [rawName, rawValue] of eachHeader(headers)) {
    const name = rawName.toLowerCase();
    const value = String(rawValue);
    if (name === 'connection') {
      connections.push(value);
      continue;
    }
    if (HOP_BY_HOP.has(name) || name.startsWith('x-relay-')) continue;
    if (ws && WS_HANDSHAKE.has(name)) continue;
    out.push([name, value]);
  }
  // Connection-named headers must go whether they came before or after Connection itself.
  for (const value of connections) {
    for (const named of value.split(',')) {
      const drop = named.trim().toLowerCase();
      for (let at = out.length - 1; at >= 0; at--) if (out[at][0] === drop) out.splice(at, 1);
    }
  }
  return out;
}

const looksLikeId = (seg) =>
  /^\d+$/.test(seg) || UUID.test(seg) || (seg.length >= 8 && HEX.test(seg)) ||
  // Long mixed letter/digit tokens are opaque ids too (base32/base64-ish salts, sha names, …).
  (seg.length >= 16 && TOKEN.test(seg) && /\d/.test(seg) && /[a-z]/i.test(seg));

/** The path as it may appear in logs: query dropped, id-shaped segments replaced, nothing else changed. */
export function pathTemplate(path) {
  const parts = path.split('?')[0].split('/').map((seg) => (seg && looksLikeId(seg) ? ':id' : seg));
  return parts.join('/') || '/';
}

/** The exact bytes the mothership signs for a hello: nonce ‖ install_id ‖ ts (ts as String of the JSON value). */
export function helloMessage(nonce, installId, ts) {
  return nonce + installId + String(ts);
}
