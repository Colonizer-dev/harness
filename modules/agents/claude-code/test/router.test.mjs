import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createServer as createTcpServer } from 'node:net';
import { test } from 'node:test';

import {
  classifyUpstreamError,
  classifyUpstreamStatus,
  fallbackBody,
  isConnectionReset,
  parseAccountRoute,
  parseRoutes,
  routeEnv,
  routingPlan,
  startRouter,
  stripOauthBetas,
  upstreamTimeouts,
} from '../router.mjs';

/** A fake upstream that records requests and answers with `respond(req, body, res)`. */
async function upstream(respond = (req, body, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ ok: true }));
}) {
  const requests = [];
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString('utf8');
    requests.push({ method: req.method, url: req.url, headers: req.headers, body });
    await respond(req, body, res);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    requests,
    close: () => new Promise((resolve) => { server.close(resolve); server.closeAllConnections(); }),
  };
}

const claudeHeaders = {
  'content-type': 'application/json',
  authorization: 'Bearer oauth-access-token',
  'x-api-key': 'sk-ant-should-not-leak',
  'anthropic-version': '2023-06-01',
  'anthropic-beta': 'oauth-2025-04-20, fine-grained-tool-streaming-2025-05-14',
};

test('routes prefixed models to the provider with its credential and a rewritten model', async () => {
  const provider = await upstream();
  const anthropic = await upstream();
  const route = { provider: 'deepseek', prefix: 'deepseek/', base_url: `${provider.url}/anthropic`, auth: 'x-api-key', key_env: 'KEY_DS' };
  const router = await startRouter({ routes: [route], env: { KEY_DS: 'placeholder-ds' }, anthropicBase: anthropic.url });
  try {
    const res = await fetch(`${router.url}/v1/messages?beta=true`, {
      method: 'POST',
      headers: claudeHeaders,
      body: JSON.stringify({ model: 'deepseek/deepseek-flash', max_tokens: 10, messages: [] }),
    });
    assert.equal(res.status, 200);
    assert.equal(anthropic.requests.length, 0);
    const [seen] = provider.requests;
    assert.equal(seen.url, '/anthropic/v1/messages?beta=true');
    assert.equal(JSON.parse(seen.body).model, 'deepseek-flash');
    assert.equal(seen.headers['x-api-key'], 'placeholder-ds');
    assert.equal(seen.headers.authorization, undefined);
    assert.equal(seen.headers['anthropic-beta'], 'fine-grained-tool-streaming-2025-05-14');
    assert.equal(seen.headers['anthropic-version'], '2023-06-01');
  } finally {
    await router.close();
    await provider.close();
    await anthropic.close();
  }
});

test('bearer and none credential modes', async () => {
  const provider = await upstream();
  const routes = [
    { provider: 'b', prefix: 'b/', base_url: provider.url, auth: 'bearer', key_env: 'KEY_B' },
    { provider: 'n', prefix: 'n/', base_url: provider.url, auth: 'none', key_env: 'KEY_B' },
  ];
  const router = await startRouter({ routes, env: { KEY_B: 'placeholder-b' }, anthropicBase: 'http://127.0.0.1:1' });
  try {
    for (const model of ['b/m', 'n/m']) {
      await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model }) });
    }
    const [bearer, none] = provider.requests;
    assert.equal(bearer.headers.authorization, 'Bearer placeholder-b');
    assert.equal(bearer.headers['x-api-key'], undefined);
    assert.equal(none.headers.authorization, undefined);
    assert.equal(none.headers['x-api-key'], undefined);
  } finally {
    await router.close();
    await provider.close();
  }
});

test('Anthropic models pass through with headers and body unchanged', async () => {
  const anthropic = await upstream();
  const router = await startRouter({ routes: [{ provider: 'x', prefix: 'x/', base_url: 'http://127.0.0.1:1', auth: 'none' }], env: {}, anthropicBase: anthropic.url });
  try {
    const body = JSON.stringify({ model: 'claude-opus-5', messages: [{ role: 'user', content: 'hi' }] });
    await fetch(`${router.url}/v1/messages?beta=true`, { method: 'POST', headers: claudeHeaders, body });
    const [seen] = anthropic.requests;
    assert.equal(seen.url, '/v1/messages?beta=true');
    assert.equal(seen.body, body);
    assert.equal(seen.headers.authorization, 'Bearer oauth-access-token');
    assert.equal(seen.headers['x-api-key'], 'sk-ant-should-not-leak');
    assert.equal(seen.headers['anthropic-beta'], claudeHeaders['anthropic-beta']);
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('streams SSE incrementally', async () => {
  let release;
  const released = new Promise((resolve) => { release = resolve; });
  const provider = await upstream(async (req, body, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('event: message_start\ndata: {"type":"message_start"}\n\n');
    await released; // only continue once the client has seen the first event
    res.end('event: message_stop\ndata: {"type":"message_stop"}\n\n');
  });
  const router = await startRouter({ routes: [{ provider: 'p', prefix: 'p/', base_url: provider.url, auth: 'none' }], env: {} });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'p/m', stream: true }) });
    assert.equal(res.headers.get('content-type'), 'text/event-stream');
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    const first = await Promise.race([
      reader.read(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('router buffered the stream')), 3000)),
    ]);
    assert.match(decoder.decode(first.value), /message_start/);
    release();
    let rest = '';
    for (let chunk = await reader.read(); !chunk.done; chunk = await reader.read()) rest += decoder.decode(chunk.value);
    assert.match(rest, /message_stop/);
  } finally {
    await router.close();
    await provider.close();
  }
});

test('count_tokens falls back to an estimate when the provider lacks it', async () => {
  const provider = await upstream((req, body, res) => {
    res.writeHead(404, { 'content-type': 'application/json' });
    res.end('{"error":"not found"}');
  });
  const router = await startRouter({ routes: [{ provider: 'p', prefix: 'p/', base_url: provider.url, auth: 'none' }], env: {} });
  try {
    const body = JSON.stringify({ model: 'p/m', messages: [{ role: 'user', content: 'x'.repeat(100) }] });
    const res = await fetch(`${router.url}/v1/messages/count_tokens?beta=true`, { method: 'POST', body });
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), { input_tokens: Math.ceil(body.length / 4) });
  } finally {
    await router.close();
    await provider.close();
  }
});

test('unreachable upstream answers 502 with an Anthropic-style error', async () => {
  const closed = await upstream();
  const deadUrl = closed.url;
  await closed.close();
  const router = await startRouter({ routes: [{ provider: 'gone', prefix: 'gone/', base_url: deadUrl, auth: 'none' }], env: {} });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'gone/m' }) });
    assert.equal(res.status, 502);
    const payload = await res.json();
    assert.equal(payload.type, 'error');
    assert.equal(payload.error.type, 'api_error');
    assert.match(payload.error.message, /gone/);
  } finally {
    await router.close();
  }
});

test('route parsing, OAuth beta stripping and the routing plan', () => {
  assert.equal(stripOauthBetas('oauth-2025-04-20'), '');
  assert.equal(stripOauthBetas('a-1, oauth-x,b-2'), 'a-1,b-2');

  assert.deepEqual(parseRoutes('not json').routes, []);
  assert.equal(parseRoutes('{}').warnings.length, 1);
  const parsed = parseRoutes(JSON.stringify([
    { prefix: 'deepseek/', base_url: 'https://api.deepseek.com/anthropic', key_env: 'K' },
    { prefix: 'bad', base_url: 'https://x' },
  ]));
  assert.equal(parsed.routes.length, 1);
  assert.deepEqual(parsed.routes[0], {
    provider: 'deepseek',
    prefix: 'deepseek/',
    base_url: 'https://api.deepseek.com/anthropic',
    auth: 'x-api-key',
    key_env: 'K',
    headers: {},
    timeout_secs: null,
    context_tokens: null,
    fallback_model: null,
  });
  assert.equal(parsed.warnings.length, 1);

  assert.equal(routingPlan({ COLONIZER_MODEL: 'opus' }).needsRouter, false);
  assert.equal(routingPlan({ COLONIZER_MODEL_ROUTES: 'oops' }).warnings.length, 1);
  const unrouted = routingPlan({ COLONIZER_SUBAGENT_MODEL: 'deepseek/deepseek-flash' });
  assert.equal(unrouted.needsRouter, true);
  assert.match(unrouted.warnings[0], /without a route/);
  const routed = routingPlan({
    COLONIZER_SUBAGENT_MODEL: 'deepseek/deepseek-flash',
    COLONIZER_MODEL_ROUTES: JSON.stringify([{ prefix: 'deepseek/', base_url: 'https://api.deepseek.com/anthropic' }]),
  });
  assert.equal(routed.needsRouter, true);
  assert.deepEqual(routed.warnings, []);
});

const gatewayRoute = (url, extra = {}) => ({
  provider: 'strix',
  prefix: 'strix/',
  base_url: `${url}/providers/strix`,
  auth: 'none',
  headers: { 'x-colonizer-colony': 'colony-token' },
  fallback_model: 'claude-sonnet-5',
  ...extra,
});

test('gateway routes carry the colony token and never the Claude credential', async () => {
  const gateway = await upstream();
  const router = await startRouter({ routes: [gatewayRoute(gateway.url)], env: {}, anthropicBase: 'http://127.0.0.1:1' });
  try {
    const res = await fetch(`${router.url}/v1/messages?beta=true`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'strix/deepseek-v4-flash' }) });
    assert.equal(res.status, 200);
    const [seen] = gateway.requests;
    assert.equal(seen.url, '/providers/strix/v1/messages?beta=true');
    assert.equal(seen.headers['x-colonizer-colony'], 'colony-token');
    assert.equal(seen.headers.authorization, undefined);
    assert.equal(seen.headers['x-api-key'], undefined);
    assert.equal(JSON.parse(seen.body).model, 'deepseek-v4-flash');
  } finally {
    await router.close();
    await gateway.close();
  }
});

test('falls back to the Claude model when the gateway reports the provider unavailable', async () => {
  const gateway = await upstream((req, body, res) => {
    res.writeHead(503, { 'content-type': 'application/json', 'x-colonizer-fallback': 'queue_timeout' });
    res.end('{"type":"error","error":{"type":"overloaded_error","message":"busy"}}');
  });
  const anthropic = await upstream();
  const logs = [];
  const router = await startRouter({ routes: [gatewayRoute(gateway.url)], env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages?beta=true`, {
      method: 'POST',
      headers: claudeHeaders,
      body: JSON.stringify({ model: 'strix/deepseek-v4-flash', thinking: { type: 'enabled', budget_tokens: 2000 }, messages: [] }),
    });
    assert.equal(res.status, 200);
    const [seen] = anthropic.requests;
    assert.equal(seen.url, '/v1/messages?beta=true');
    assert.deepEqual(JSON.parse(seen.body), { model: 'claude-sonnet-5', thinking: { type: 'adaptive' }, messages: [] });
    // The fallback uses Claude Code's own credential and betas, and not the colony token.
    assert.equal(seen.headers.authorization, 'Bearer oauth-access-token');
    assert.equal(seen.headers['anthropic-beta'], claudeHeaders['anthropic-beta']);
    assert.equal(seen.headers['x-colonizer-colony'], undefined);
    assert.deepEqual(logs, [{ level: 'warn', message: 'provider strix unavailable (queue_timeout); used claude-sonnet-5', source: 'model_router' }]);
  } finally {
    await router.close();
    await gateway.close();
    await anthropic.close();
  }
});

test('falls back when the gateway itself is unreachable', async () => {
  const closed = await upstream();
  const deadUrl = closed.url;
  await closed.close();
  const anthropic = await upstream();
  const logs = [];
  const router = await startRouter({ routes: [gatewayRoute(deadUrl)], env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'strix/m' }) });
    assert.equal(res.status, 200);
    assert.equal(JSON.parse(anthropic.requests[0].body).model, 'claude-sonnet-5');
    assert.match(logs[0].message, /^upstream failure: provider=strix class=connect status=502 elapsed=[\d.]+s model=m detail=ECONNREFUSED$/);
    assert.match(logs[1].message, /gateway unreachable/);
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('falls back when the gateway marks a 429/403 as quota exhaustion, and only then', async () => {
  const gateway = await upstream((req, body, res) => {
    const marked = JSON.parse(body).model === 'marked';
    res.writeHead(429, { 'content-type': 'application/json', ...(marked ? { 'x-colonizer-fallback': 'provider_quota_exhausted' } : {}) });
    res.end('{"type":"error","error":{"type":"rate_limit_error","message":"quota exhausted"}}');
  });
  const anthropic = await upstream();
  const routes = [gatewayRoute(gateway.url), gatewayRoute(gateway.url, { provider: 'nofb', prefix: 'nofb/', fallback_model: null })];
  const router = await startRouter({ routes, env: {}, anthropicBase: anthropic.url });
  try {
    const quota = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'strix/marked' }) });
    assert.equal(quota.status, 200);
    assert.equal(JSON.parse(anthropic.requests[0].body).model, 'claude-sonnet-5');
    const bare = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'strix/unmarked' }) });
    assert.equal(bare.status, 429);
    assert.equal(anthropic.requests.length, 1);
    const noFallback = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'nofb/marked' }) });
    assert.equal(noFallback.status, 429);
    assert.equal(anthropic.requests.length, 1);
  } finally {
    await router.close();
    await gateway.close();
    await anthropic.close();
  }
});

test('provider errors without the gateway marker, or routes without a fallback, pass through', async () => {
  const gateway = await upstream((req, body, res) => {
    const marked = JSON.parse(body).model === 'marked';
    res.writeHead(marked ? 503 : 502, { 'content-type': 'application/json', ...(marked ? { 'x-colonizer-fallback': 'unreachable' } : {}) });
    res.end('{"type":"error","error":{"type":"api_error","message":"upstream"}}');
  });
  const anthropic = await upstream();
  const routes = [gatewayRoute(gateway.url), gatewayRoute(gateway.url, { provider: 'nofb', prefix: 'nofb/', fallback_model: null })];
  const router = await startRouter({ routes, env: {}, anthropicBase: anthropic.url });
  try {
    const unmarked = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'strix/unmarked' }) });
    assert.equal(unmarked.status, 502);
    const noFallback = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'nofb/marked' }) });
    assert.equal(noFallback.status, 503);
    assert.equal((await noFallback.json()).error.message, 'upstream');
    assert.equal(anthropic.requests.length, 0);
  } finally {
    await router.close();
    await gateway.close();
    await anthropic.close();
  }
});

test('route settings become Claude Code timeouts and a context limit for used routes only', () => {
  const routes = [
    { prefix: 'strix/', timeout_secs: 900, context_tokens: 131072 },
    { prefix: 'deepseek/', timeout_secs: 600, context_tokens: 1000000 },
    { prefix: 'huge/', timeout_secs: 3600, context_tokens: 4096 },
  ];
  assert.deepEqual(routeEnv(routes, { COLONIZER_MODEL: 'opus' }), {});
  assert.deepEqual(routeEnv(routes, { COLONIZER_MODEL: 'opus', COLONIZER_SUBAGENT_MODEL: 'strix/deepseek-v4-flash', COLONIZER_BACKGROUND_MODEL: 'deepseek/deepseek-flash' }), {
    CLAUDE_STREAM_IDLE_TIMEOUT_MS: '900000',
    API_TIMEOUT_MS: '960000',
    API_FORCE_IDLE_TIMEOUT: '0',
    CLAUDE_ASYNC_AGENT_STALL_TIMEOUT_MS: '900000',
    CLAUDE_CODE_MAX_CONTEXT_TOKENS: '131072',
  });
  // The stream idle timeout is capped at Claude Code's 30-minute maximum.
  assert.equal(routeEnv(routes, { COLONIZER_SUBAGENT_MODEL: 'huge/m' }).CLAUDE_STREAM_IDLE_TIMEOUT_MS, '1800000');
  // Fast routes keep Claude Code's defaults.
  assert.deepEqual(routeEnv([{ prefix: 'fast/', timeout_secs: 120 }], { COLONIZER_SUBAGENT_MODEL: 'fast/m' }), {});

  assert.deepEqual(fallbackBody({ model: 'x', thinking: { type: 'adaptive' } }, 'claude-sonnet-5'), { model: 'claude-sonnet-5', thinking: { type: 'adaptive' } });
  assert.deepEqual(parseRoutes(JSON.stringify([{ prefix: 'p/', base_url: 'http://h', auth: 'none', headers: { 'X-Colonizer-Colony': 't', 'bad header': 'x', n: 1 }, timeout_secs: 900, context_tokens: -1 }])).routes[0].headers, { 'x-colonizer-colony': 't' });
});

// Issue #983: long generations must not be cut off, and what does fail must be named for what it is.

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** An Anthropic stand-in that thinks quietly for `thinkMs`, then streams `events` events `gapMs` apart. */
const slowAnthropic = ({ thinkMs = 0, gapMs, events }) =>
  upstream(async (req, body, res) => {
    await sleep(thinkMs);
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('event: message_start\ndata: {"type":"message_start"}\n\n');
    for (let i = 0; i < events; i += 1) {
      await sleep(gapMs);
      if (res.destroyed) return;
      res.write(`event: content_block_delta\ndata: {"type":"content_block_delta","index":${i}}\n\n`);
    }
    res.end('event: message_stop\ndata: {"type":"message_stop"}\n\n');
  });

test('a stream that runs far longer than the idle timeout, never silent that long, goes through whole', async () => {
  // Scaled down: a 400 ms idle timeout stands in for the 600 s default, and the answer takes about
  // 1.9 s (more than four idle timeouts), as an Opus answer outlasted the old fixed limits.
  const anthropic = await slowAnthropic({ thinkMs: 250, gapMs: 150, events: 11 });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, idleTimeoutMs: 400, log: (entry) => logs.push(entry) });
  try {
    const startedAt = Date.now();
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5', stream: true }) });
    assert.equal(res.status, 200);
    const text = await res.text();
    assert.ok(Date.now() - startedAt > 4 * 400, 'the answer outlasted several idle timeouts');
    assert.equal(text.match(/event: content_block_delta/g).length, 11);
    assert.match(text, /message_stop/);
    assert.deepEqual(logs, []);
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('an upstream silent past the idle timeout before answering is a 504 timeout, not "unreachable"', async () => {
  const anthropic = await slowAnthropic({ thinkMs: 1500, gapMs: 0, events: 0 });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, idleTimeoutMs: 300, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5', stream: true }) });
    assert.equal(res.status, 504);
    const payload = await res.json();
    assert.equal(payload.error.type, 'timeout_error');
    assert.equal(payload.error.message, 'model router: Anthropic timed out after 0.3 s without sending anything');
    assert.doesNotMatch(payload.error.message, /unreachable/);
    assert.equal(logs.length, 1);
    assert.equal(logs[0].source, 'model_router');
    assert.equal(logs[0].level, 'error');
    assert.match(logs[0].message, /^upstream failure: provider=anthropic class=timeout status=504 elapsed=[\d.]+s model=claude-opus-5-5 detail=UND_ERR_HEADERS_TIMEOUT$/);
    // A timeout is never replayed.
    assert.equal(anthropic.requests.length, 1);
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('a stream that goes silent past the idle timeout ends with an SSE timeout error', async () => {
  const anthropic = await slowAnthropic({ gapMs: 1500, events: 1 });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, idleTimeoutMs: 300, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-sonnet-5', stream: true }) });
    assert.equal(res.status, 200);
    const text = await res.text();
    assert.match(text, /message_start/);
    const event = JSON.parse(text.split('event: error\ndata: ')[1]);
    assert.deepEqual(event, { type: 'error', error: { type: 'timeout_error', message: 'model router: Anthropic timed out after 0.3 s without sending anything' } });
    assert.match(logs[0].message, /class=timeout status=504 .* detail=UND_ERR_BODY_TIMEOUT$/);
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('Anthropic error answers pass through unchanged, and are logged by class', async () => {
  const answers = {
    'auth': [401, {}, { type: 'authentication_error', message: 'invalid x-api-key' }],
    'limit': [429, { 'retry-after': '17' }, { type: 'rate_limit_error', message: 'usage limit reached' }],
    'over': [529, {}, { type: 'overloaded_error', message: 'Overloaded' }],
  };
  const anthropic = await upstream((req, body, res) => {
    const [status, headers, error] = answers[JSON.parse(body).model];
    res.writeHead(status, { 'content-type': 'application/json', ...headers });
    res.end(JSON.stringify({ type: 'error', error }));
  });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    for (const [model, [status, headers, error]] of Object.entries(answers)) {
      const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model }) });
      assert.equal(res.status, status);
      assert.equal(res.headers.get('retry-after'), headers['retry-after'] ?? null);
      assert.deepEqual(await res.json(), { type: 'error', error });
    }
    assert.deepEqual(
      logs.map((entry) => [entry.level, entry.message.replace(/elapsed=[\d.]+s/, 'elapsed=Xs')]),
      [
        ['error', 'upstream failure: provider=anthropic class=auth status=401 elapsed=Xs model=auth detail=authentication_error'],
        ['warn', 'upstream failure: provider=anthropic class=rate_limit status=429 elapsed=Xs model=limit detail=rate_limit_error retry-after=17'],
        ['error', 'upstream failure: provider=anthropic class=upstream_5xx status=529 elapsed=Xs model=over detail=overloaded_error'],
      ],
    );
    // No credential reaches the log.
    assert.ok(logs.every((entry) => !/oauth-access-token|sk-ant/.test(entry.message)));
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('Anthropic refusing the connection is "unreachable", and says why', async () => {
  const closed = await upstream();
  const deadUrl = closed.url;
  await closed.close();
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: deadUrl, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5' }) });
    assert.equal(res.status, 502);
    assert.deepEqual(await res.json(), { type: 'error', error: { type: 'api_error', message: 'model router: Anthropic is unreachable (connection failed: ECONNREFUSED)' } });
    assert.match(logs[0].message, /class=connect status=502 .* detail=ECONNREFUSED$/);
  } finally {
    await router.close();
  }
});

test('upstream failures are classified by cause', () => {
  const fetchFailed = (cause) => new TypeError('fetch failed', { cause });
  const coded = (code, message = code) => Object.assign(new Error(message), { code });
  const opts = { target: 'Anthropic', connectMs: 30_000, idleMs: 600_000 };
  const cases = [
    [coded('ENOTFOUND'), 'dns', 502, 'model router: Anthropic is unreachable (DNS lookup failed: ENOTFOUND)'],
    [coded('EAI_AGAIN'), 'dns', 502, 'model router: Anthropic is unreachable (DNS lookup failed: EAI_AGAIN)'],
    [coded('UND_ERR_CONNECT_TIMEOUT'), 'connect', 502, 'model router: Anthropic is unreachable (no connection within 30 s)'],
    [coded('ECONNREFUSED'), 'connect', 502, 'model router: Anthropic is unreachable (connection failed: ECONNREFUSED)'],
    [Object.assign(new AggregateError([coded('ENETUNREACH'), coded('EHOSTUNREACH')]), { code: undefined }), 'connect', 502, 'model router: Anthropic is unreachable (connection failed: EHOSTUNREACH)'],
    [coded('UNABLE_TO_VERIFY_LEAF_SIGNATURE'), 'tls', 502, 'model router: Anthropic is unreachable (TLS failed: UNABLE_TO_VERIFY_LEAF_SIGNATURE)'],
    [coded('ERR_TLS_CERT_ALTNAME_INVALID'), 'tls', 502, 'model router: Anthropic is unreachable (TLS failed: ERR_TLS_CERT_ALTNAME_INVALID)'],
    [coded('UND_ERR_HEADERS_TIMEOUT'), 'timeout', 504, 'model router: Anthropic timed out after 600 s without sending anything'],
    [coded('UND_ERR_BODY_TIMEOUT'), 'timeout', 504, 'model router: Anthropic timed out after 600 s without sending anything'],
    [coded('ECONNRESET'), 'connection', 502, 'model router: the connection to Anthropic failed (ECONNRESET)'],
    [coded('UND_ERR_SOCKET', 'other side closed'), 'connection', 502, 'model router: the connection to Anthropic failed (UND_ERR_SOCKET)'],
    [new Error('something odd'), 'connection', 502, 'model router: the connection to Anthropic failed (something odd)'],
  ];
  for (const [cause, expectedClass, status, message] of cases) {
    const failure = classifyUpstreamError(fetchFailed(cause), opts);
    assert.equal(failure.class, expectedClass, message);
    assert.equal(failure.status, status, message);
    assert.equal(failure.message, message);
  }

  assert.equal(classifyUpstreamStatus(401), 'auth');
  assert.equal(classifyUpstreamStatus(403, 'permission_error'), 'auth');
  assert.equal(classifyUpstreamStatus(429), 'rate_limit');
  assert.equal(classifyUpstreamStatus(400, 'rate_limit_error'), 'rate_limit');
  assert.equal(classifyUpstreamStatus(500), 'upstream_5xx');
  assert.equal(classifyUpstreamStatus(529, 'overloaded_error'), 'upstream_5xx');
  assert.equal(classifyUpstreamStatus(413, 'request_too_large'), 'client_error');
  assert.equal(classifyUpstreamStatus(200), null);
});

test('upstream timeouts: a 30 s connect, and an idle timeout of at least 10 minutes', () => {
  assert.deepEqual(upstreamTimeouts({}), { connectMs: 30_000, idleMs: 600_000 });
  assert.deepEqual(upstreamTimeouts({ COLONIZER_ROUTER_CONNECT_TIMEOUT_SECS: '10', COLONIZER_ROUTER_IDLE_TIMEOUT_SECS: '1800' }), { connectMs: 10_000, idleMs: 1_800_000 });
  assert.deepEqual(upstreamTimeouts({ COLONIZER_ROUTER_IDLE_TIMEOUT_SECS: 'soon' }), { connectMs: 30_000, idleMs: 600_000 });
  // A slow provider's own timeout_secs is never undercut.
  assert.equal(upstreamTimeouts({}, [{ timeout_secs: 900 }, { timeout_secs: null }]).idleMs, 900_000);
});

// The keep-alive race: a pooled socket the other side had already closed fails the next request on it
// at once with UND_ERR_SOCKET "other side closed". Nothing was answered, so it is sent once more.

/** A raw TCP upstream: `onRequest(socket, n)` runs once the n-th request (1-based) has fully arrived. */
async function rawUpstream(onRequest) {
  let requests = 0;
  const server = createTcpServer((socket) => {
    let seen = '';
    socket.on('data', (chunk) => {
      seen += chunk.toString('latin1');
      const end = seen.indexOf('\r\n\r\n');
      if (end < 0) return;
      const length = Number(/content-length: *(\d+)/i.exec(seen.slice(0, end))?.[1] ?? 0);
      if (seen.length < end + 4 + length) return;
      seen = '';
      requests += 1;
      onRequest(socket, requests);
    });
    socket.on('error', () => {});
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    requests: () => requests,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

const okAnswer = (body) => `HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;

test('a request whose connection is closed before any answer is retried once on a new connection', async () => {
  const bodies = [];
  const anthropic = await upstream(async (req, body, res) => {
    bodies.push(body);
    if (bodies.length === 1) {
      req.socket.destroy(); // the keep-alive race: closed under the request, nothing written
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ ok: true }));
  });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    const body = JSON.stringify({ model: 'claude-opus-5-5', messages: [{ role: 'user', content: 'hi' }] });
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body });
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), { ok: true });
    // The same request, body and credential headers both times.
    assert.deepEqual(bodies, [body, body]);
    assert.equal(anthropic.requests[1].headers.authorization, 'Bearer oauth-access-token');
    assert.equal(logs.length, 1);
    assert.equal(logs[0].source, 'model_router');
    assert.equal(logs[0].level, 'warn');
    assert.match(logs[0].message, /^upstream retry: provider=anthropic class=connection_retry elapsed=[\d.]+s model=claude-opus-5-5 detail=UND_ERR_SOCKET$/);
    assert.ok(logs.every((entry) => !/oauth-access-token|sk-ant/.test(entry.message)));
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('a routed provider request is retried the same way', async () => {
  const provider = await rawUpstream((socket, n) => {
    if (n === 1) socket.destroy();
    else socket.end(okAnswer('{"ok":true}'));
  });
  const logs = [];
  const route = { provider: 'p', prefix: 'p/', base_url: provider.url, auth: 'none' };
  const router = await startRouter({ routes: [route], env: {}, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', body: JSON.stringify({ model: 'p/m' }) });
    assert.equal(res.status, 200);
    assert.equal(provider.requests(), 2);
    assert.match(logs[0].message, /^upstream retry: provider=p class=connection_retry .* model=m detail=UND_ERR_SOCKET$/);
    assert.equal(logs.length, 1);
  } finally {
    await router.close();
    await provider.close();
  }
});

test('a retry that fails the same way is one 502 naming UND_ERR_SOCKET, with no third attempt', async () => {
  const anthropic = await rawUpstream((socket) => socket.destroy());
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5' }) });
    assert.equal(res.status, 502);
    assert.deepEqual(await res.json(), { type: 'error', error: { type: 'api_error', message: 'model router: the connection to Anthropic failed (UND_ERR_SOCKET)' } });
    await sleep(100); // room for a third attempt, were there one
    assert.equal(anthropic.requests(), 2);
    assert.deepEqual(
      logs.map((entry) => [entry.level, entry.message.replace(/elapsed=[\d.]+s/, 'elapsed=Xs')]),
      [
        ['warn', 'upstream retry: provider=anthropic class=connection_retry elapsed=Xs model=claude-opus-5-5 detail=UND_ERR_SOCKET'],
        ['error', 'upstream failure: provider=anthropic class=connection status=502 elapsed=Xs model=claude-opus-5-5 detail=UND_ERR_SOCKET'],
      ],
    );
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('no retry once any byte of the answer has arrived', async () => {
  // Part of the status and headers, then a close: undici still says UND_ERR_SOCKET, but the upstream
  // had begun to answer, so the request may have been acted on.
  for (const partial of ['HTTP/1.1 2', 'HTTP/1.1 200 OK\r\ncontent-type: appl']) {
    const anthropic = await rawUpstream((socket) => socket.end(partial));
    const logs = [];
    const router = await startRouter({ env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
    try {
      const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5' }) });
      assert.equal(res.status, 502, partial);
      await res.arrayBuffer();
      await sleep(50);
      assert.equal(anthropic.requests(), 1, partial);
      assert.equal(logs.length, 1, partial);
      assert.match(logs[0].message, /^upstream failure: .* class=connection status=502 .* detail=UND_ERR_SOCKET$/, partial);
    } finally {
      await router.close();
      await anthropic.close();
    }
  }
});

test('no retry once the response headers have been sent', async () => {
  const anthropic = await rawUpstream((socket) => {
    socket.write('HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n');
    setTimeout(() => socket.destroy(), 50);
  });
  const logs = [];
  const router = await startRouter({ env: {}, anthropicBase: anthropic.url, log: (entry) => logs.push(entry) });
  try {
    const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5', stream: true }) });
    assert.equal(res.status, 200);
    const text = await res.text();
    assert.match(text, /event: error/);
    await sleep(50);
    assert.equal(anthropic.requests(), 1);
    assert.ok(logs.every((entry) => !/connection_retry/.test(entry.message)));
  } finally {
    await router.close();
    await anthropic.close();
  }
});

test('only a reset with no answer received counts as the keep-alive race', () => {
  const coded = (code, message = code) => Object.assign(new Error(message), { code });
  // Untagged (no byte count known, as with an injected fetchImpl): never.
  assert.equal(isConnectionReset(new TypeError('fetch failed', { cause: coded('UND_ERR_SOCKET') })), false);
  assert.equal(isConnectionReset(coded('ECONNRESET')), false);
});

/** A fake mothership account-route endpoint whose answer the test changes between requests. */
async function accountEndpoint(answer) {
  const state = { answer, asked: 0, headers: null };
  const server = await upstream((req, body, res) => {
    state.asked += 1;
    state.headers = req.headers;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(state.answer));
  });
  return { state, url: `${server.url}/account-route`, close: server.close };
}

test('parseAccountRoute accepts an http(s) url with headers and ignores anything else', () => {
  assert.deepEqual(parseAccountRoute('{"url":"http://h:1/account-route","headers":{"x-colonizer-colony":"t"}}'), {
    url: 'http://h:1/account-route',
    headers: { 'x-colonizer-colony': 't' },
  });
  for (const bad of ['', 'nope', '{}', '{"url":"ftp://h"}', '[]']) assert.equal(parseAccountRoute(bad), null, bad);
  assert.equal(routingPlan({ COLONIZER_ACCOUNT_ROUTE: '{"url":"http://h/account-route"}' }).needsRouter, true);
  assert.equal(routingPlan({ COLONIZER_ACCOUNT_ROUTE: 'junk' }).needsRouter, false);
});

test('with the Claude account out, a Claude request goes to the fallback route; the reset sends it back', async () => {
  const provider = await upstream();
  const anthropic = await upstream();
  const endpoint = await accountEndpoint({ action: 'fallback', model: 'minimax/MiniMax-M3.1' });
  const route = { provider: 'minimax', prefix: 'minimax/', base_url: `${provider.url}/anthropic`, auth: 'none', headers: { 'x-colonizer-colony': 'tok' } };
  const logs = [];
  const router = await startRouter({
    routes: [route],
    env: {},
    anthropicBase: anthropic.url,
    log: (line) => logs.push(line),
    accountRoute: { url: endpoint.url, headers: { 'x-colonizer-colony': 'tok' } },
  });
  const ask = () => fetch(`${router.url}/v1/messages`, {
    method: 'POST',
    headers: claudeHeaders,
    body: JSON.stringify({ model: 'claude-opus-5-5', max_tokens: 10, messages: [] }),
  });
  try {
    assert.equal((await ask()).status, 200);
    assert.equal(anthropic.requests.length, 0, 'nothing went to Anthropic while the account is out');
    const [seen] = provider.requests;
    assert.equal(seen.url, '/anthropic/v1/messages');
    assert.equal(JSON.parse(seen.body).model, 'MiniMax-M3.1');
    assert.equal(seen.headers.authorization, undefined, 'the Anthropic credential never reaches the provider');
    assert.equal(seen.headers['x-colonizer-colony'], 'tok');
    assert.equal(endpoint.state.headers['x-colonizer-colony'], 'tok');
    assert.ok(logs.some((line) => /Claude account is out; Claude requests go to minimax\/MiniMax-M3\.1/.test(line.message)));

    // The answer is reused for a few seconds, not asked for on every request.
    await ask();
    assert.equal(endpoint.state.asked, 1);
  } finally {
    await router.close();
  }

  // The reset: the mothership says Claude again, and the same request goes to Anthropic.
  endpoint.state.answer = { action: 'claude' };
  const back = await startRouter({
    routes: [route],
    env: {},
    anthropicBase: anthropic.url,
    accountRoute: { url: endpoint.url, headers: {} },
  });
  try {
    const res = await fetch(`${back.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'claude-opus-5-5', messages: [] }) });
    assert.equal(res.status, 200);
    assert.equal(anthropic.requests.length, 1);
    assert.equal(JSON.parse(anthropic.requests[0].body).model, 'claude-opus-5-5');
    assert.equal(anthropic.requests[0].headers.authorization, 'Bearer oauth-access-token');
  } finally {
    await back.close();
    await endpoint.close();
    await provider.close();
    await anthropic.close();
  }
});

test('a parked answer, a missing route or an unreachable mothership leave the request on Anthropic', async () => {
  const provider = await upstream();
  const anthropic = await upstream();
  const route = { provider: 'minimax', prefix: 'minimax/', base_url: provider.url, auth: 'none' };
  const cases = [
    { action: 'parked', reason: 'needs a trusted provider' },
    { action: 'fallback', model: 'elsewhere/model' },
    null,
  ];
  for (const answer of cases) {
    const endpoint = answer ? await accountEndpoint(answer) : null;
    const router = await startRouter({
      routes: [route],
      env: {},
      anthropicBase: anthropic.url,
      accountRoute: { url: endpoint?.url ?? 'http://127.0.0.1:1/account-route', headers: {} },
    });
    try {
      const res = await fetch(`${router.url}/v1/messages`, { method: 'POST', headers: claudeHeaders, body: JSON.stringify({ model: 'sonnet', messages: [] }) });
      assert.equal(res.status, 200, JSON.stringify(answer));
    } finally {
      await router.close();
      await endpoint?.close();
    }
  }
  assert.equal(provider.requests.length, 0);
  assert.equal(anthropic.requests.length, cases.length);
  await provider.close();
  await anthropic.close();
});
