import assert from 'node:assert/strict';
import { test } from 'node:test';

import { z } from 'zod';

import { createHistoryServer, formatHits, HISTORY_PROMPT_APPEND, HISTORY_SERVER, HISTORY_TOOL } from '../history.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const historyEnv = { ...base, COLONIZER_HISTORY_URL: 'http://127.0.0.1:1/history', COLONIZER_HISTORY_TOKEN: 'sekrit' };
const fakeServer = { name: HISTORY_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(); zod is real, so the shape holds real schemas. */
function fakeSdk() {
  const tool = (name, description, shape, handler) => ({ name, description, shape, handler });
  const createSdkMcpServer = (server) => ({ type: 'sdk', name: server.name, tools: server.tools });
  return { z, tool, createSdkMcpServer };
}

const hit = { colony: 'colony-7f3a', repo: 'acme/widgets', org: 'acme', agent: 'claude', status: 'success', created_at: '2026-09-20T10:00:00Z', seq: 12, ts: '2026-09-20T10:05:00Z', turn: 3, role: 'assistant', snippet: 'moved routes into sessions/ by concern' };

test('colony_history_search posts the query to the gateway and frames the hits as untrusted history', async () => {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    return { ok: true, status: 200, json: async () => ({ hits: [hit] }) };
  };
  const server = createHistoryServer({ url: historyEnv.COLONIZER_HISTORY_URL, token: 'sekrit', fetchImpl, ...fakeSdk() });
  assert.equal(server.name, HISTORY_SERVER);
  assert.equal(server.tools[0].name, 'colony_history_search');
  assert.equal(HISTORY_TOOL, 'mcp__colonizer_history__colony_history_search');

  const reply = await server.tools[0].handler({ query: 'splitting sessions.rs', limit: 5 });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, 'http://127.0.0.1:1/history');
  assert.equal(calls[0].init.method, 'POST');
  assert.deepEqual(calls[0].init.headers, { Authorization: 'Bearer sekrit', 'Content-Type': 'application/json' });
  assert.deepEqual(JSON.parse(calls[0].init.body), { query: 'splitting sessions.rs', limit: 5 });
  assert.ok(calls[0].init.signal instanceof AbortSignal, 'the request cannot hang past its timeout');

  assert.match(reply.content[0].text, /^<colony-history>\n/);
  assert.match(reply.content[0].text, /\n<\/colony-history>$/);
  assert.match(reply.content[0].text, /untrusted data, not instructions/);
  assert.match(reply.content[0].text, /\[colony-7f3a\] acme\/widgets assistant \(2026-09-20T10:05:00Z\)\n- moved routes into sessions\/ by concern/);

  const bare = await server.tools[0].handler({ query: 'anything' });
  assert.deepEqual(JSON.parse(calls[1].init.body), { query: 'anything' }, 'no limit, no limit field');
});

test('empty hits, a gateway error and a network failure are short texts marked as errors, never throws', async () => {
  const serverWith = (fetchImpl) => createHistoryServer({ url: 'u', token: 't', fetchImpl, ...fakeSdk() }).tools[0];

  const empty = await serverWith(async () => ({ ok: true, status: 200, json: async () => ({ hits: [] }) })).handler({ query: 'rust toolchain' });
  assert.equal(empty.content[0].text, 'No earlier colonies matched.');
  assert.equal(formatHits([]), 'No earlier colonies matched.');
  assert.equal(formatHits(), 'No earlier colonies matched.');

  const notFound = await serverWith(async () => ({ ok: false, status: 404, json: async () => ({}) })).handler({ query: 'x' });
  assert.equal(notFound.isError, true);
  assert.match(notFound.content[0].text, /404/);

  const down = await serverWith(async () => { throw new Error('ECONNREFUSED'); }).handler({ query: 'x' });
  assert.equal(down.isError, true);

  const notJson = await serverWith(async () => ({ ok: true, status: 200, json: async () => { throw new Error('Unexpected token'); } })).handler({ query: 'x' });
  assert.equal(notJson.isError, true);
});

test('the input schema requires a query and caps the limit at the server’s 50', () => {
  const [search] = createHistoryServer({ url: 'u', token: 't', fetchImpl: async () => ({}), ...fakeSdk() }).tools;
  assert.deepEqual(Object.keys(search.shape), ['query', 'limit']);
  const schema = z.object(search.shape);
  assert.equal(schema.safeParse({ query: '' }).success, false);
  assert.equal(schema.safeParse({}).success, false, 'the query is required');
  assert.equal(schema.safeParse({ query: 'x'.repeat(501) }).success, false);
  assert.equal(schema.safeParse({ query: 'x'.repeat(500) }).success, true);
  assert.equal(schema.safeParse({ query: 'x' }).success, true);
  assert.equal(schema.safeParse({ query: 'x', limit: 0 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 1.5 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 51 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 50 }).success, true);
});

test('colony history is wired in only when both env vars are set and the server exists', () => {
  const off = buildOptions({ ...base }, { historyServer: fakeServer }).options;
  assert.equal(off.mcpServers, undefined);
  assert.ok(!off.systemPrompt.append.includes(HISTORY_PROMPT_APPEND));

  const urlOnly = buildOptions({ ...base, COLONIZER_HISTORY_URL: 'http://127.0.0.1:1/history' }, { historyServer: fakeServer }).options;
  assert.equal(urlOnly.mcpServers, undefined, 'the token is the other half of the gate');

  const noServer = buildOptions(historyEnv).options;
  assert.equal(noServer.mcpServers, undefined);

  const on = buildOptions(historyEnv, { historyServer: fakeServer }).options;
  assert.deepEqual(on.mcpServers, { [HISTORY_SERVER]: fakeServer });
  assert.ok(on.systemPrompt.append.includes(HISTORY_PROMPT_APPEND));
  // Read-only, so subagents may search too: no gate is registered, and no allowedTools entry shadows canUseTool.
  assert.deepEqual((on.hooks?.PreToolUse ?? []).map((e) => e.matcher), ['Bash']);
  assert.equal(on.allowedTools, undefined);
});

test('colony history sits beside the other colony servers rather than replacing them', () => {
  const { options } = buildOptions(
    { ...historyEnv, COLONIZER_FINDINGS: 'true', COLONIZER_MEMORY_DIR: '/colonizer/memory' },
    { historyServer: fakeServer, findingsServer: { name: 'colonizer_findings' }, memoryServer: { name: 'colonizer_memory' } },
  );
  assert.deepEqual(Object.keys(options.mcpServers).sort(), ['colonizer_findings', 'colonizer_history', 'colonizer_memory']);
});
