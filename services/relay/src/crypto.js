// The relay's cryptography helpers, kept free of Workers APIs so they run on plain Node (node:crypto's
// WebCrypto is the same standard the Workers runtime implements). Three jobs live here:
//
//   - base64 in the two shapes the protocol uses: standard (public keys, signatures) and base64url
//     (nonces, tokens, cookie payloads), decoding either and always throwing on garbage;
//   - Ed25519 verification of mothership signatures over `METHOD\npath\nts\nbody`;
//   - HMAC-SHA256 sealing of the sign-in state and session cookies, verified with crypto.subtle.verify
//     so the comparison is constant time.

const BASE64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const VALUE = new Map([...BASE64].map((c, i) => [c, i]));
// The URL-safe alphabet differs in exactly two characters; both decode to the same bits.
VALUE.set('-', VALUE.get('+'));
VALUE.set('_', VALUE.get('/'));

const ENCODER = new TextEncoder();

/**
 * Decodes standard or URL-safe base64, padding optional, into bytes. Throws on anything else —
 * callers turn that into a 400, never into stored data.
 */
export function b64decode(s) {
  if (typeof s !== 'string') throw new TypeError('not base64');
  let body = s;
  for (let pad = 0; body.endsWith('=') && pad < 2; pad++) body = body.slice(0, -1);
  if (body.includes('=') || body.length % 4 === 1) throw new TypeError('not base64');
  const out = new Uint8Array(Math.floor((body.length * 3) / 4));
  let acc = 0;
  let bits = 0;
  let o = 0;
  for (const ch of body) {
    const v = VALUE.get(ch);
    if (v === undefined) throw new TypeError('not base64');
    acc = (acc << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (acc >>> bits) & 0xff;
    }
  }
  return out;
}

/** Encodes bytes as standard base64, padding included. */
export function b64encode(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i];
    const b1 = bytes[i + 1];
    const b2 = bytes[i + 2];
    s += BASE64[b0 >> 2];
    s += BASE64[((b0 & 3) << 4) | ((b1 ?? 0) >> 4)];
    s += b1 === undefined ? '=' : BASE64[((b1 & 15) << 2) | ((b2 ?? 0) >> 6)];
    s += b2 === undefined ? '=' : BASE64[b2 & 63];
  }
  return s;
}

/** Encodes bytes as base64url without padding: the nonce, token and cookie-payload shape. */
export function b64urlEncode(bytes) {
  return b64encode(bytes).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

/**
 * Verifies an Ed25519 signature (base64) over a UTF-8 message against a base64 public key. Every
 * failure — a bad key, a short signature, an unsupported runtime — is just `false`: authentication
 * must never turn a malformed input into a 500.
 */
export async function verifyEd25519(publicKeyB64, message, sigB64) {
  try {
    const key = await crypto.subtle.importKey('raw', b64decode(publicKeyB64), { name: 'Ed25519' }, false, ['verify']);
    return (
      (await crypto.subtle.verify({ name: 'Ed25519' }, key, b64decode(sigB64), ENCODER.encode(message))) === true
    );
  } catch {
    return false;
  }
}

/** `bytes` random bytes as base64url without padding: nonce and token shape. */
export function randomToken(bytes = 16) {
  return b64urlEncode(crypto.getRandomValues(new Uint8Array(bytes)));
}

async function hmacKey(secret) {
  // TextEncoder turns undefined into the string "undefined": a missing secret must throw, so nothing is
  // ever sealed under — or verified against — a key anyone can compute.
  if (typeof secret !== 'string' || secret.length === 0) throw new TypeError('no secret');
  return crypto.subtle.importKey('raw', ENCODER.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, [
    'sign',
    'verify',
  ]);
}

/** HMAC-SHA256 of a string, base64url: the tag half of a cookie value. */
export async function hmacSign(secret, value) {
  return b64urlEncode(new Uint8Array(await crypto.subtle.sign('HMAC', await hmacKey(secret), ENCODER.encode(value))));
}

/** Constant-time HMAC check (crypto.subtle.verify), false on any malformed input. */
export async function hmacVerify(secret, value, tag) {
  if (typeof tag !== 'string' || tag.length === 0) return false;
  try {
    return await crypto.subtle.verify('HMAC', await hmacKey(secret), b64decode(tag), ENCODER.encode(value));
  } catch {
    return false;
  }
}
