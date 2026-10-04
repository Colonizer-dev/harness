// Local model router (docs/protocol.md §6.1, §6.5). Claude Code's ANTHROPIC_BASE_URL points here;
// requests for `<provider>/<model>` go to that provider's route (normally the mothership's provider
// gateway), and everything else passes through to Anthropic untouched. When the gateway reports that a
// provider is unavailable, the request is retried on the route's Claude fallback model.

import { createServer } from 'node:http';
import { Readable } from 'node:stream';

import { Agent, fetch as undiciFetch } from 'undici';

const AUTH_MODES = new Set(['x-api-key', 'bearer', 'none']);
const PROVIDER_PREFIX = /^[A-Za-z0-9][A-Za-z0-9._-]*\//;
const HEADER_NAME = /^[A-Za-z0-9-]+$/;
const MODEL_VARS = ['COLONIZER_MODEL', 'COLONIZER_SUBAGENT_MODEL', 'COLONIZER_BACKGROUND_MODEL'];
// Hop-by-hop and length/encoding headers are recomputed by fetch and node:http.
const DROP_REQUEST = new Set(['connection', 'keep-alive', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'host', 'content-length', 'accept-encoding']);
const DROP_RESPONSE = new Set(['connection', 'keep-alive', 'transfer-encoding', 'content-length', 'content-encoding']);
const FALLBACK_HEADER = 'x-colonizer-fallback';
const FALLBACK_STATUSES = new Set([502, 503, 504]);
// Claude Code caps its byte-level stream idle timeout at 30 minutes.
const MAX_STREAM_IDLE_MS = 30 * 60_000;
// Claude Code's own defaults already cover requests up to this long.
const DEFAULT_TIMEOUT_SECS = 300;
// Upstream timeouts (issue #983). There is no total-request timeout: a streamed answer runs as long as
// the model keeps writing. What is bounded is opening the connection, and how long the upstream may
// stay silent — before its response headers (a long non-streamed answer, or thinking before the first
// event) and between two chunks of the body. Node's built-in fetch caps both silences at 300 s.
const DEFAULT_CONNECT_TIMEOUT_SECS = 30;
const DEFAULT_IDLE_TIMEOUT_SECS = 600;
export const LOG_SOURCE = 'model_router';

const positiveInt = (value) => (Number.isInteger(value) && value > 0 ? value : null);

function routeHeaders(value) {
  const out = {};
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    for (const [name, header] of Object.entries(value)) {
      if (HEADER_NAME.test(name) && typeof header === 'string') out[name.toLowerCase()] = header;
    }
  }
  return out;
}

/** COLONIZER_MODEL_ROUTES → validated routes, with warnings instead of throwing. */
export function parseRoutes(raw) {
  let data;
  try {
    data = JSON.parse(raw);
  } catch {
    return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: not valid JSON; using Anthropic only'] };
  }
  if (!Array.isArray(data)) {
    return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: expected a JSON array; using Anthropic only'] };
  }
  const routes = [];
  const warnings = [];
  data.forEach((entry, i) => {
    const prefix = typeof entry?.prefix === 'string' ? entry.prefix : '';
    const baseUrl = typeof entry?.base_url === 'string' ? entry.base_url : '';
    const auth = entry?.auth ?? 'x-api-key';
    if (!prefix.endsWith('/') || prefix.length < 2 || !/^https?:\/\//.test(baseUrl) || !AUTH_MODES.has(auth)) {
      warnings.push(`ignoring model route ${i}: needs a "<provider>/" prefix, an http(s) base_url and auth x-api-key|bearer|none`);
      return;
    }
    routes.push({
      provider: typeof entry.provider === 'string' && entry.provider ? entry.provider : prefix.slice(0, -1),
      prefix,
      base_url: baseUrl,
      auth,
      key_env: typeof entry.key_env === 'string' && entry.key_env ? entry.key_env : null,
      headers: routeHeaders(entry.headers),
      timeout_secs: positiveInt(entry.timeout_secs),
      context_tokens: positiveInt(entry.context_tokens),
      fallback_model: typeof entry.fallback_model === 'string' && entry.fallback_model ? entry.fallback_model : null,
    });
  });
  return { routes, warnings };
}

/** Decides from the environment whether a router is needed, and which routes it serves. */
export function routingPlan(env = process.env) {
  const warnings = [];
  let routes = [];
  if (env.COLONIZER_MODEL_ROUTES?.trim()) {
    const parsed = parseRoutes(env.COLONIZER_MODEL_ROUTES);
    routes = parsed.routes;
    warnings.push(...parsed.warnings);
  }
  const prefixed = MODEL_VARS.filter((key) => PROVIDER_PREFIX.test(env[key] ?? ''));
  for (const key of prefixed) {
    if (!routes.some((route) => env[key].startsWith(route.prefix))) {
      warnings.push(`${key}=${env[key]} names a provider without a route; those requests will go to Anthropic`);
    }
  }
  return { routes, warnings, needsRouter: routes.length > 0 || prefixed.length > 0 };
}

/**
 * Claude Code settings for the routes the configured models use: longer timeouts for slow providers
 * and the smallest context window among them (§6.5).
 */
export function routeEnv(routes, env = process.env) {
  const models = MODEL_VARS.map((key) => env[key]).filter(Boolean);
  const used = routes.filter((route) => models.some((model) => model.startsWith(route.prefix)));
  const out = {};
  const timeoutSecs = Math.max(0, ...used.map((route) => route.timeout_secs ?? 0));
  if (timeoutSecs > DEFAULT_TIMEOUT_SECS) {
    const ms = timeoutSecs * 1000;
    out.CLAUDE_STREAM_IDLE_TIMEOUT_MS = String(Math.min(ms, MAX_STREAM_IDLE_MS));
    out.API_TIMEOUT_MS = String(ms + 60_000);
    out.API_FORCE_IDLE_TIMEOUT = '0';
    out.CLAUDE_ASYNC_AGENT_STALL_TIMEOUT_MS = String(ms);
  }
  const contexts = used.map((route) => route.context_tokens).filter(Boolean);
  if (contexts.length) out.CLAUDE_CODE_MAX_CONTEXT_TOKENS = String(Math.min(...contexts));
  return out;
}

/** Removes OAuth capability betas, which only Anthropic understands. */
export function stripOauthBetas(value) {
  return String(value)
    .split(',')
    .map((beta) => beta.trim())
    .filter((beta) => beta && !beta.startsWith('oauth-'))
    .join(',');
}

/** The request body for a Claude fallback: the fallback model, and thinking in a form Claude accepts. */
export function fallbackBody(parsed, model) {
  const body = { ...parsed, model };
  if (body.thinking?.type === 'enabled') body.thinking = { type: 'adaptive' };
  return body;
}

/**
 * The router's upstream timeouts in milliseconds: `COLONIZER_ROUTER_CONNECT_TIMEOUT_SECS` (default 30)
 * and `COLONIZER_ROUTER_IDLE_TIMEOUT_SECS` (default 600, the module's "Model request idle timeout"
 * setting). The idle timeout is never shorter than a route's own `timeout_secs`, which already sets
 * Claude Code's timeouts for that provider (§6.5).
 */
export function upstreamTimeouts(env = process.env, routes = []) {
  const secs = (name, fallback) => {
    const value = Number(env[name]);
    return Number.isFinite(value) && value > 0 ? value : fallback;
  };
  const connectMs = secs('COLONIZER_ROUTER_CONNECT_TIMEOUT_SECS', DEFAULT_CONNECT_TIMEOUT_SECS) * 1000;
  const routeMs = Math.max(0, ...routes.map((route) => (route.timeout_secs ?? 0) * 1000));
  const idleMs = Math.max(secs('COLONIZER_ROUTER_IDLE_TIMEOUT_SECS', DEFAULT_IDLE_TIMEOUT_SECS) * 1000, routeMs);
  return { connectMs, idleMs };
}

const DNS_CODES = new Set(['ENOTFOUND', 'EAI_AGAIN', 'EAI_FAIL', 'EAI_NONAME', 'EAI_NODATA']);
const CONNECT_CODES = new Set(['ECONNREFUSED', 'EHOSTUNREACH', 'ENETUNREACH', 'EHOSTDOWN', 'ENETDOWN', 'EADDRNOTAVAIL', 'ETIMEDOUT']);
const TLS_CODE = /^(ERR_TLS_|ERR_SSL_|ERR_OSSL_|CERT_|UNABLE_TO_|SELF_SIGNED_|DEPTH_ZERO_|HOSTNAME_MISMATCH)/;
const IDLE_CODES = new Set(['UND_ERR_HEADERS_TIMEOUT', 'UND_ERR_BODY_TIMEOUT']);

/** The most specific error code in a fetch failure: fetch wraps the socket's error in `cause`. */
function errorCode(err) {
  const seen = new Set();
  const queue = [err];
  let found = null;
  while (queue.length) {
    const e = queue.shift();
    if (!e || typeof e !== 'object' || seen.has(e)) continue;
    seen.add(e);
    if (typeof e.code === 'string') found = e.code;
    if (e.cause) queue.push(e.cause);
    if (Array.isArray(e.errors)) queue.push(...e.errors);
  }
  return found;
}

function rootMessage(err) {
  let e = err;
  while (e?.cause && typeof e.cause === 'object') e = e.cause;
  return String(e?.message ?? e ?? 'unknown error').slice(0, 200);
}

const seconds = (ms) => `${Number((ms / 1000).toFixed(1))} s`;

/**
 * What an upstream request that failed without an HTTP answer means, and what Claude Code is told
 * (issue #983). Only a failure to open the connection is "unreachable"; a silence past the idle timeout
 * is a 504 that says so, and a connection that broke after it was open is said as such.
 * @returns {{class: string, status: number, detail: string, message: string}}
 */
export function classifyUpstreamError(err, { target = 'the upstream', connectMs = DEFAULT_CONNECT_TIMEOUT_SECS * 1000, idleMs = DEFAULT_IDLE_TIMEOUT_SECS * 1000 } = {}) {
  const code = errorCode(err);
  const detail = code ?? rootMessage(err);
  if (code && IDLE_CODES.has(code)) {
    return { class: 'timeout', status: 504, detail, message: `model router: ${target} timed out after ${seconds(idleMs)} without sending anything` };
  }
  if (code === 'UND_ERR_CONNECT_TIMEOUT') {
    return { class: 'connect', status: 502, detail, message: `model router: ${target} is unreachable (no connection within ${seconds(connectMs)})` };
  }
  if (code && DNS_CODES.has(code)) {
    return { class: 'dns', status: 502, detail, message: `model router: ${target} is unreachable (DNS lookup failed: ${code})` };
  }
  if (code && CONNECT_CODES.has(code)) {
    return { class: 'connect', status: 502, detail, message: `model router: ${target} is unreachable (connection failed: ${code})` };
  }
  if (code && TLS_CODE.test(code)) {
    return { class: 'tls', status: 502, detail, message: `model router: ${target} is unreachable (TLS failed: ${code})` };
  }
  return { class: 'connection', status: 502, detail, message: `model router: the connection to ${target} failed (${detail})` };
}

/** The class of an upstream HTTP error answer, for the log; the answer itself passes through as sent. */
export function classifyUpstreamStatus(status, errorType = null) {
  if (status === 401 || status === 403) return errorType === 'rate_limit_error' ? 'rate_limit' : 'auth';
  if (status === 429 || errorType === 'rate_limit_error') return 'rate_limit';
  if (status >= 500) return 'upstream_5xx';
  if (status >= 400) return 'client_error';
  return null;
}

function errorType(text) {
  try {
    const type = JSON.parse(text)?.error?.type;
    return typeof type === 'string' && /^[a-z_]{1,64}$/.test(type) ? type : null;
  } catch {
    return null;
  }
}

function joinUrl(base, path) {
  return base.replace(/\/+$/, '') + path;
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
  res.end(body);
}

function sendError(res, status, message, type = 'api_error') {
  sendJson(res, status, { type: 'error', error: { type, message } });
}

/** Anthropic's error type for a router-made answer, so Claude Code reads a timeout as one. */
const errorTypeFor = (failure) => (failure.class === 'timeout' ? 'timeout_error' : 'api_error');

/**
 * Starts the router on 127.0.0.1 with an ephemeral port.
 * @param {object} [args]
 * @param {Function} [args.log]  receives `{level, message, source}` for fallbacks and upstream failures
 * @param {Function} [args.fetchImpl]  replaces the upstream client (and its timeouts) entirely
 * @param {number} [args.connectTimeoutMs]  overrides `upstreamTimeouts(env, routes).connectMs`
 * @param {number} [args.idleTimeoutMs]  overrides `upstreamTimeouts(env, routes).idleMs`
 * @returns {Promise<{url: string, close: () => Promise<void>}>}
 */
export async function startRouter({
  routes = [],
  env = process.env,
  anthropicBase = 'https://api.anthropic.com',
  fetchImpl,
  log = () => {},
  connectTimeoutMs,
  idleTimeoutMs,
} = {}) {
  const timeouts = upstreamTimeouts(env, routes);
  const connectMs = connectTimeoutMs ?? timeouts.connectMs;
  const idleMs = idleTimeoutMs ?? timeouts.idleMs;
  // Our own pool rather than fetch's global one, whose 300 s header and body timeouts cut off long
  // generations. No total timeout: undici has none unless asked.
  const dispatcher = fetchImpl ? null : new Agent({ connect: { timeout: connectMs }, headersTimeout: idleMs, bodyTimeout: idleMs });
  const upstreamFetch = fetchImpl ?? ((url, init) => undiciFetch(url, { ...init, dispatcher }));
  const say = (level, message) => log({ level, message, source: LOG_SOURCE });
  // One line per failed upstream request, for the mothership's log (issue #983). The colony never holds
  // the real credential, so there is none to leak here; the mothership adds the account it chose.
  const reportFailure = ({ provider, model, failureClass, status, started, detail }) => {
    const elapsed = ((Date.now() - started) / 1000).toFixed(1);
    const parts = [`upstream failure: provider=${provider}`, `class=${failureClass}`, `status=${status}`, `elapsed=${elapsed}s`];
    if (model) parts.push(`model=${model}`);
    if (detail) parts.push(`detail=${detail}`);
    say(failureClass === 'rate_limit' || failureClass === 'client_error' ? 'warn' : 'error', parts.join(' '));
  };

  const server = createServer(async (req, res) => {
    const started = Date.now();
    const abort = new AbortController();
    res.on('close', () => abort.abort());
    try {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const body = Buffer.concat(chunks);

      let parsed = null;
      if (body.length && body[0] === 0x7b) {
        try {
          parsed = JSON.parse(body.toString('utf8'));
        } catch {
          parsed = null;
        }
      }
      const model = typeof parsed?.model === 'string' ? parsed.model : null;
      let route = model ? routes.find((r) => model.startsWith(r.prefix)) : undefined;
      // Who answers this request, and which model, for the log.
      let provider = route ? route.provider : 'anthropic';
      let upstreamModel = route ? model.slice(route.prefix.length) : model;

      const headers = {};
      for (const [name, value] of Object.entries(req.headers)) {
        if (!DROP_REQUEST.has(name) && value !== undefined) headers[name] = Array.isArray(value) ? value.join(', ') : value;
      }
      const send = (target, sendHeaders, sendBody) =>
        upstreamFetch(target, {
          method: req.method,
          headers: sendHeaders,
          body: req.method === 'GET' || req.method === 'HEAD' ? undefined : sendBody,
          redirect: 'manual',
          signal: abort.signal,
        });
      // Claude Code's own headers go to Anthropic unchanged, as for any unrouted model.
      const toAnthropic = (sendBody) => send(joinUrl(anthropicBase, req.url), headers, sendBody);
      // A request that got no HTTP answer: logged, then told to Claude Code as what it was.
      const failed = (err, target) => {
        const failure = classifyUpstreamError(err, { target, connectMs, idleMs });
        reportFailure({ provider, model: upstreamModel, failureClass: failure.class, status: failure.status, started, detail: failure.detail });
        return failure;
      };
      const answerFailure = (failure) => {
        if (!res.headersSent && !abort.signal.aborted) sendError(res, failure.status, failure.message, errorTypeFor(failure));
      };

      let upstream;
      if (route) {
        parsed.model = upstreamModel;
        const routed = { ...headers };
        delete routed.authorization;
        delete routed['x-api-key'];
        if (routed['anthropic-beta'] !== undefined) {
          const betas = stripOauthBetas(routed['anthropic-beta']);
          if (betas) routed['anthropic-beta'] = betas;
          else delete routed['anthropic-beta'];
        }
        const key = route.key_env ? env[route.key_env] : undefined;
        if (key && route.auth === 'x-api-key') routed['x-api-key'] = key;
        if (key && route.auth === 'bearer') routed.authorization = `Bearer ${key}`;
        Object.assign(routed, route.headers);

        let reason = null;
        let failure = null;
        try {
          upstream = await send(joinUrl(route.base_url, req.url), routed, Buffer.from(JSON.stringify(parsed)));
          if (FALLBACK_STATUSES.has(upstream.status)) reason = upstream.headers.get(FALLBACK_HEADER);
          // Quota exhaustion answers 429/403, never 502/503/504: the gateway names it in the
          // header, and only that marker (never a bare 429/403) earns the Claude retry.
          if (!reason && (upstream.status === 429 || upstream.status === 403)) {
            const quota = upstream.headers.get(FALLBACK_HEADER);
            if (quota === 'provider_quota_exhausted') reason = quota;
          }
        } catch (err) {
          if (abort.signal.aborted) return;
          failure = failed(err, `provider "${route.provider}"`);
          reason = failure.class === 'timeout' ? 'gateway timed out' : 'gateway unreachable';
          upstream = null;
        }
        if (reason && route.fallback_model) {
          await upstream?.body?.cancel().catch(() => {});
          say('warn', `provider ${route.provider} unavailable (${reason}); used ${route.fallback_model}`);
          const fallback = fallbackBody(parsed, route.fallback_model);
          route = undefined;
          provider = 'anthropic';
          upstreamModel = fallback.model;
          try {
            upstream = await toAnthropic(Buffer.from(JSON.stringify(fallback)));
          } catch (err) {
            if (abort.signal.aborted) return;
            answerFailure(failed(err, 'Anthropic'));
            return;
          }
        } else if (!upstream) {
          answerFailure(failure);
          return;
        }
      } else {
        try {
          upstream = await toAnthropic(body);
        } catch (err) {
          if (abort.signal.aborted) return;
          answerFailure(failed(err, 'Anthropic'));
          return;
        }
      }

      const path = req.url.split('?')[0];
      if (route && path.endsWith('/messages/count_tokens') && [404, 405, 501].includes(upstream.status)) {
        await upstream.body?.cancel().catch(() => {});
        sendJson(res, 200, { input_tokens: Math.ceil(body.toString('utf8').length / 4) });
        return;
      }

      const outHeaders = {};
      upstream.headers.forEach((value, name) => {
        if (!DROP_RESPONSE.has(name)) outHeaders[name] = value;
      });
      if (upstream.status >= 400) {
        // An error answer is small: read it to name its class in the log, then pass it on byte for
        // byte, with its status and headers (retry-after included), so Claude Code sees exactly what
        // the upstream said — an expired login, a rate or usage limit, an overload.
        const answer = upstream.body ? Buffer.from(await upstream.arrayBuffer()) : Buffer.alloc(0);
        const type = errorType(answer.toString('utf8'));
        const retryAfter = upstream.headers.get('retry-after');
        const detail = [type, retryAfter ? `retry-after=${retryAfter}` : null].filter(Boolean).join(' ');
        reportFailure({ provider, model: upstreamModel, failureClass: classifyUpstreamStatus(upstream.status, type), status: upstream.status, started, detail });
        res.writeHead(upstream.status, outHeaders);
        res.end(answer);
        return;
      }
      res.writeHead(upstream.status, outHeaders);
      if (!upstream.body) {
        res.end();
        return;
      }
      // Streamed through as it arrives: nothing is buffered, and no timer bounds the whole answer.
      const sse = (upstream.headers.get('content-type') ?? '').includes('text/event-stream');
      Readable.fromWeb(upstream.body)
        .on('error', (err) => {
          if (abort.signal.aborted) {
            res.destroy();
            return;
          }
          const failure = failed(err, provider === 'anthropic' ? 'Anthropic' : `provider "${provider}"`);
          if (sse && !res.writableEnded) {
            // An SSE error event, as Anthropic itself ends a stream that fails, so Claude Code reports
            // the cause instead of a bare dropped connection.
            const event = { type: 'error', error: { type: errorTypeFor(failure), message: failure.message } };
            res.end(`event: error\ndata: ${JSON.stringify(event)}\n\n`);
          } else {
            res.destroy();
          }
        })
        .pipe(res);
    } catch {
      if (!res.headersSent) sendError(res, 500, 'model router: internal error');
      else res.destroy();
    }
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const { port } = server.address();
  return {
    url: `http://127.0.0.1:${port}`,
    close: async () => {
      await new Promise((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections?.();
      });
      await dispatcher?.destroy().catch(() => {});
    },
  };
}
