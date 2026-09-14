import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';

import { parseRoutes, routingPlan, startRouter, stripOauthBetas } from '../router.mjs';

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
  assert.deepEqual(parsed.routes[0], { provider: 'deepseek', prefix: 'deepseek/', base_url: 'https://api.deepseek.com/anthropic', auth: 'x-api-key', key_env: 'K' });
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
