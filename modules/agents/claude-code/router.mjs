// Local model router (docs/protocol.md §6.1). Claude Code's ANTHROPIC_BASE_URL points here; requests
// for `<provider>/<model>` go to that provider's Anthropic-compatible endpoint with its credential, and
// everything else passes through to Anthropic untouched.

import { createServer } from 'node:http';
import { Readable } from 'node:stream';

const AUTH_MODES = new Set(['x-api-key', 'bearer', 'none']);
const PROVIDER_PREFIX = /^[A-Za-z0-9][A-Za-z0-9._-]*\//;
const MODEL_VARS = ['COLONIZER_MODEL', 'COLONIZER_SUBAGENT_MODEL', 'COLONIZER_BACKGROUND_MODEL'];
// Hop-by-hop and length/encoding headers are recomputed by fetch and node:http.
const DROP_REQUEST = new Set(['connection', 'keep-alive', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'host', 'content-length', 'accept-encoding']);
const DROP_RESPONSE = new Set(['connection', 'keep-alive', 'transfer-encoding', 'content-length', 'content-encoding']);

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

/** Removes OAuth capability betas, which only Anthropic understands. */
export function stripOauthBetas(value) {
  return String(value)
    .split(',')
    .map((beta) => beta.trim())
    .filter((beta) => beta && !beta.startsWith('oauth-'))
    .join(',');
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
 * @returns {Promise<{url: string, close: () => Promise<void>}>}
 */
export async function startRouter({ routes = [], env = process.env, anthropicBase = 'https://api.anthropic.com', fetchImpl = fetch } = {}) {
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
      const route = model ? routes.find((r) => model.startsWith(r.prefix)) : undefined;

      const headers = {};
      for (const [name, value] of Object.entries(req.headers)) {
        if (!DROP_REQUEST.has(name) && value !== undefined) headers[name] = Array.isArray(value) ? value.join(', ') : value;
      }

      let target;
      let outBody = body;
      if (route) {
        parsed.model = model.slice(route.prefix.length);
        outBody = Buffer.from(JSON.stringify(parsed));
        delete headers.authorization;
        delete headers['x-api-key'];
        if (headers['anthropic-beta'] !== undefined) {
          const betas = stripOauthBetas(headers['anthropic-beta']);
          if (betas) headers['anthropic-beta'] = betas;
          else delete headers['anthropic-beta'];
        }
        const key = route.key_env ? env[route.key_env] : undefined;
        if (key && route.auth === 'x-api-key') headers['x-api-key'] = key;
        if (key && route.auth === 'bearer') headers.authorization = `Bearer ${key}`;
        target = joinUrl(route.base_url, req.url);
      } else {
        target = joinUrl(anthropicBase, req.url);
      }

      let upstream;
      try {
        upstream = await fetchImpl(target, {
          method: req.method,
          headers,
          body: req.method === 'GET' || req.method === 'HEAD' ? undefined : outBody,
          redirect: 'manual',
          signal: abort.signal,
        });
      } catch {
        if (!res.headersSent && !abort.signal.aborted) {
          sendError(res, 502, `model router: ${route ? `provider "${route.provider}"` : 'Anthropic'} is unreachable`);
        }
        return;
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
