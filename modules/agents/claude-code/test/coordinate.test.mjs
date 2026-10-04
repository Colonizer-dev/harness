import assert from 'node:assert/strict';
import { test } from 'node:test';

import { z } from 'zod';

import { COORDINATION_PROMPT_APPEND, COORDINATION_SERVER, createCoordinationServer } from '../coordinate.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const gatewayEnv = { ...base, COLONIZER_COORD_URL: 'http://127.0.0.1:1/coordinate', COLONIZER_COORD_TOKEN: 'sekrit' };
const fakeServer = { name: COORDINATION_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(); zod is real, so the shape holds real schemas. */
function fakeSdk() {
  const tool = (name, description, shape, handler) => ({ name, description, shape, handler });
  const createSdkMcpServer = (server) => ({ type: 'sdk', name: server.name, tools: server.tools });
  return { z, tool, createSdkMcpServer };
}

/** A server whose tool handlers post to a stub returning `reply`, capturing every call. */
function serverWith(reply) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    return typeof reply === 'function' ? reply() : reply;
  };
  const server = createCoordinationServer({ url: 'http://127.0.0.1:1/coordinate', token: 'sekrit', fetchImpl, ...fakeSdk() });
  return { server, calls };
}

const ok = (body) => ({ ok: true, status: 200, json: async () => body, text: async () => JSON.stringify(body) });

test('the four tools post op bodies to the coordinate URL with the bearer token', async () => {
  const { server, calls } = serverWith(ok({ claim: { holders: [] } }));
  assert.equal(server.name, COORDINATION_SERVER);
  assert.deepEqual(server.tools.map((t) => t.name), ['claim', 'claims', 'send', 'inbox']);

  await server.tools[0].handler({ paths: ['crates/a.rs', 'web/b.ts'], reason: 'issue 834' });
  assert.deepEqual(JSON.parse(calls[0].init.body), { op: 'claim', paths: ['crates/a.rs', 'web/b.ts'], reason: 'issue 834' });

  await server.tools[1].handler({});
  assert.deepEqual(JSON.parse(calls[1].init.body), { op: 'claims' });

  await server.tools[2].handler({ to: '831', text: 'please take crates/a.rs' });
  assert.deepEqual(JSON.parse(calls[2].init.body), { op: 'send', to: '831', text: 'please take crates/a.rs' });

  await server.tools[3].handler({});
  assert.deepEqual(JSON.parse(calls[3].init.body), { op: 'inbox' });

  for (const { url, init } of calls) {
    assert.equal(url, 'http://127.0.0.1:1/coordinate');
    assert.equal(init.method, 'POST');
    assert.deepEqual(init.headers, { Authorization: 'Bearer sekrit', 'Content-Type': 'application/json' });
    assert.ok(init.signal instanceof AbortSignal, 'the request cannot hang past its timeout');
  }
});

test('a 200 body comes back as pretty JSON text', async () => {
  const body = { claims: [{ colony: '831', paths: ['crates/a.rs'] }] };
  const { server } = serverWith(ok(body));
  const reply = await server.tools[1].handler({});
  assert.equal(reply.content[0].text, JSON.stringify(body, null, 2));
  assert.match(reply.content[0].text, /\n {2}"claims"/);
  assert.equal(reply.isError, undefined);
});

test('a non-2xx answer and a network failure are short errors, never throws', async () => {
  const throttled = await serverWith({ ok: false, status: 429, text: async () => 'slow down' }).server.tools[0].handler({ paths: ['a'], reason: 'r' });
  assert.equal(throttled.isError, true);
  assert.match(throttled.content[0].text, /429/);

  const down = await serverWith(async () => { throw new Error('ECONNREFUSED'); }).server.tools[3].handler({});
  assert.equal(down.isError, true);
  assert.match(down.content[0].text, /unreachable/);

  const notJson = await serverWith({ ok: true, status: 200, json: async () => { throw new Error('Unexpected token'); } }).server.tools[2].handler({ to: '831', text: 'hi' });
  assert.equal(notJson.isError, true);
});

test('the input schemas cap the paths, the reason and the message', () => {
  const [claim, , send] = createCoordinationServer({ url: 'u', token: 't', fetchImpl: async () => ({}), ...fakeSdk() }).tools;
  assert.deepEqual(Object.keys(claim.shape), ['paths', 'reason']);
  const claimSchema = z.object(claim.shape);
  assert.equal(claimSchema.safeParse({ paths: [], reason: 'r' }).success, false);
  assert.equal(claimSchema.safeParse({ paths: Array.from({ length: 201 }, (_, i) => `p${i}`), reason: 'r' }).success, false);
  assert.equal(claimSchema.safeParse({ paths: Array.from({ length: 200 }, (_, i) => `p${i}`), reason: 'r' }).success, true);
  assert.equal(claimSchema.safeParse({ paths: ['a'], reason: '' }).success, false);
  assert.equal(claimSchema.safeParse({ paths: [''], reason: 'r' }).success, false);

  const sendSchema = z.object(send.shape);
  assert.equal(sendSchema.safeParse({ to: '831', text: 'x'.repeat(4001) }).success, false);
  assert.equal(sendSchema.safeParse({ to: '831', text: 'x'.repeat(4000) }).success, true);
  assert.equal(sendSchema.safeParse({ to: '', text: 'hi' }).success, false);
});

test('coordination is wired in only when the gateway env and the server are both present', () => {
  const off = buildOptions({ ...base }, { coordinateServer: fakeServer }).options;
  assert.equal(off.mcpServers, undefined);
  assert.ok(!off.systemPrompt.append.includes(COORDINATION_PROMPT_APPEND));

  const urlOnly = buildOptions({ ...base, COLONIZER_COORD_URL: 'http://127.0.0.1:1/coordinate' }, { coordinateServer: fakeServer }).options;
  assert.equal(urlOnly.mcpServers, undefined, 'the token is the other half of the gate');

  const noServer = buildOptions(gatewayEnv).options;
  assert.equal(noServer.mcpServers, undefined);

  const on = buildOptions(gatewayEnv, { coordinateServer: fakeServer }).options;
  assert.deepEqual(on.mcpServers, { [COORDINATION_SERVER]: fakeServer });
  assert.ok(on.systemPrompt.append.includes(COORDINATION_PROMPT_APPEND));
});

test('coordination sits beside the other colony servers rather than replacing them', () => {
  const { options } = buildOptions(
    {
      ...gatewayEnv,
      COLONIZER_RECALL_URL: 'http://127.0.0.1:1/recall',
      COLONIZER_RECALL_TOKEN: 'sekrit',
      COLONIZER_FINDINGS: 'true',
      COLONIZER_MEMORY_DIR: '/colonizer/memory',
    },
    { coordinateServer: fakeServer, recallServer: { name: 'colonizer_recall' }, findingsServer: { name: 'colonizer_findings' }, memoryServer: { name: 'colonizer_memory' } },
  );
  assert.deepEqual(Object.keys(options.mcpServers).sort(), ['colonizer_coord', 'colonizer_findings', 'colonizer_memory', 'colonizer_recall']);
});
