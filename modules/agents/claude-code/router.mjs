// Local model router (docs/protocol.md §6.1, §6.5). Claude Code's ANTHROPIC_BASE_URL points here;
// requests for `<provider>/<model>` go to that provider's route (normally the mothership's provider
// gateway), and everything else passes through to Anthropic untouched. When the gateway reports that a
// provider is unavailable, the request is retried on the route's Claude fallback model.

import { createServer } from 'node:http';
import { Readable } from 'node:stream';

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

function joinUrl(base, path) {
  return base.replace(/\/+$/, '') + path;
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
  res.end(body);
}

function sendError(res, status, message) {
  sendJson(res, status, { type: 'error', error: { type: 'api_error', message } });
}

/**
 * Starts the router on 127.0.0.1 with an ephemeral port.
 * @param {object} [args]
 * @param {Function} [args.log]  receives `{level, message}` for fallbacks
 * @returns {Promise<{url: string, close: () => Promise<void>}>}
 */
export async function startRouter({ routes = [], env = process.env, anthropicBase = 'https://api.anthropic.com', fetchImpl = fetch, log = () => {} } = {}) {
  const server = createServer(async (req, res) => {
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

      const headers = {};
      for (const [name, value] of Object.entries(req.headers)) {
        if (!DROP_REQUEST.has(name) && value !== undefined) headers[name] = Array.isArray(value) ? value.join(', ') : value;
      }
      const send = (target, sendHeaders, sendBody) =>
        fetchImpl(target, {
          method: req.method,
          headers: sendHeaders,
          body: req.method === 'GET' || req.method === 'HEAD' ? undefined : sendBody,
          redirect: 'manual',
          signal: abort.signal,
        });
      // Claude Code's own headers go to Anthropic unchanged, as for any unrouted model.
      const toAnthropic = (sendBody) => send(joinUrl(anthropicBase, req.url), headers, sendBody);

      let upstream;
      if (route) {
        parsed.model = model.slice(route.prefix.length);
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
        try {
          upstream = await send(joinUrl(route.base_url, req.url), routed, Buffer.from(JSON.stringify(parsed)));
          if (FALLBACK_STATUSES.has(upstream.status)) reason = upstream.headers.get(FALLBACK_HEADER);
        } catch {
          if (abort.signal.aborted) return;
          reason = 'gateway unreachable';
          upstream = null;
        }
        if (reason && route.fallback_model) {
          await upstream?.body?.cancel().catch(() => {});
          log({ level: 'warn', message: `provider ${route.provider} unavailable (${reason}); used ${route.fallback_model}` });
          const fallback = fallbackBody(parsed, route.fallback_model);
          route = undefined;
          try {
            upstream = await toAnthropic(Buffer.from(JSON.stringify(fallback)));
          } catch {
            if (!res.headersSent && !abort.signal.aborted) sendError(res, 502, 'model router: Anthropic is unreachable');
            return;
          }
        } else if (!upstream) {
          if (!res.headersSent) sendError(res, 502, `model router: provider "${route.provider}" is unreachable`);
          return;
        }
      } else {
        try {
          upstream = await toAnthropic(body);
        } catch {
          if (!res.headersSent && !abort.signal.aborted) sendError(res, 502, 'model router: Anthropic is unreachable');
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
      res.writeHead(upstream.status, outHeaders);
      if (!upstream.body) {
        res.end();
        return;
      }
      Readable.fromWeb(upstream.body)
        .on('error', () => res.destroy())
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
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections?.();
      }),
  };
}
