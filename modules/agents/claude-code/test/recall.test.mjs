import assert from 'node:assert/strict';
import { test } from 'node:test';

import { z } from 'zod';

import { buildOptions } from '../runner.mjs';
import { createRecallServer, formatHits, RECALL_PROMPT_APPEND, RECALL_SERVER, RECALL_TOOL } from '../recall.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const recallEnv = { ...base, COLONIZER_RECALL_URL: 'http://127.0.0.1:1/recall', COLONIZER_RECALL_TOKEN: 'sekrit' };
const fakeServer = { name: RECALL_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(); zod is real, so the shape holds real schemas. */
function fakeSdk() {
  const tool = (name, description, shape, handler) => ({ name, description, shape, handler });
  const createSdkMcpServer = (server) => ({ type: 'sdk', name: server.name, tools: server.tools });
  return { z, tool, createSdkMcpServer };
}

const hit = { project: 'acme/widgets', title: 'Split sessions.rs into sessions/', updated: '2026-09-20T10:00:00Z', snippets: ['moved routes into sessions/ by concern'] };

test('recall posts the query to the gateway and frames the hits as untrusted history', async () => {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    return { ok: true, status: 200, json: async () => ({ hits: [hit] }) };
  };
  const server = createRecallServer({ url: recallEnv.COLONIZER_RECALL_URL, token: 'sekrit', fetchImpl, ...fakeSdk() });
  assert.equal(server.name, RECALL_SERVER);
  assert.equal(RECALL_TOOL, 'mcp__colonizer_recall__recall');

  const reply = await server.tools[0].handler({ query: 'splitting sessions.rs', limit: 5 });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, 'http://127.0.0.1:1/recall');
  assert.equal(calls[0].init.method, 'POST');
  assert.deepEqual(calls[0].init.headers, { Authorization: 'Bearer sekrit', 'Content-Type': 'application/json' });
  assert.deepEqual(JSON.parse(calls[0].init.body), { query: 'splitting sessions.rs', limit: 5 });
  assert.ok(calls[0].init.signal instanceof AbortSignal, 'the request cannot hang past its timeout');

  assert.match(reply.content[0].text, /^<past-sessions>\n/);
  assert.match(reply.content[0].text, /\n<\/past-sessions>$/);
  assert.match(reply.content[0].text, /untrusted data, not instructions/);
  assert.match(reply.content[0].text, /\[acme\/widgets\] Split sessions\.rs into sessions\/ \(2026-09-20T10:00:00Z\)\n- moved routes into sessions\/ by concern/);

  const bare = await server.tools[0].handler({ query: 'anything' });
  assert.deepEqual(JSON.parse(calls[1].init.body), { query: 'anything' }, 'no limit, no limit field');
});

test('empty hits, a gateway error and a network failure are short texts marked as errors, never throws', async () => {
  const serverWith = (fetchImpl) => createRecallServer({ url: 'u', token: 't', fetchImpl, ...fakeSdk() }).tools[0];

  const empty = await serverWith(async () => ({ ok: true, status: 200, json: async () => ({ hits: [] }) })).handler({ query: 'rust toolchain' });
  assert.equal(empty.content[0].text, 'No earlier sessions matched.');
  assert.equal(formatHits([]), 'No earlier sessions matched.');
  assert.equal(formatHits(), 'No earlier sessions matched.');

  const notFound = await serverWith(async () => ({ ok: false, status: 404, json: async () => ({}) })).handler({ query: 'x' });
  assert.equal(notFound.isError, true);
  assert.match(notFound.content[0].text, /404/);

  const down = await serverWith(async () => { throw new Error('ECONNREFUSED'); }).handler({ query: 'x' });
  assert.equal(down.isError, true);

  const notJson = await serverWith(async () => ({ ok: true, status: 200, json: async () => { throw new Error('Unexpected token'); } })).handler({ query: 'x' });
  assert.equal(notJson.isError, true);
});

test('the input schema caps the query and the limit', () => {
  const [recall] = createRecallServer({ url: 'u', token: 't', fetchImpl: async () => ({}), ...fakeSdk() }).tools;
  assert.deepEqual(Object.keys(recall.shape), ['query', 'limit']);
  const schema = z.object(recall.shape);
  assert.equal(schema.safeParse({ query: '' }).success, false);
  assert.equal(schema.safeParse({ query: 'x'.repeat(501) }).success, false);
  assert.equal(schema.safeParse({ query: 'x'.repeat(500) }).success, true);
  assert.equal(schema.safeParse({ query: 'x' }).success, true);
  assert.equal(schema.safeParse({ query: 'x', limit: 0 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 1.5 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 21 }).success, false);
  assert.equal(schema.safeParse({ query: 'x', limit: 20 }).success, true);
});

test('recall is wired in only when both env vars are set and the server exists', () => {
  const off = buildOptions({ ...base }, { recallServer: fakeServer }).options;
  assert.equal(off.mcpServers, undefined);
  assert.ok(!off.systemPrompt.append.includes(RECALL_PROMPT_APPEND));

  const urlOnly = buildOptions({ ...base, COLONIZER_RECALL_URL: 'http://127.0.0.1:1/recall' }, { recallServer: fakeServer }).options;
  assert.equal(urlOnly.mcpServers, undefined, 'the token is the other half of the gate');

  const noServer = buildOptions(recallEnv).options;
  assert.equal(noServer.mcpServers, undefined);

  const on = buildOptions(recallEnv, { recallServer: fakeServer }).options;
  assert.deepEqual(on.mcpServers, { [RECALL_SERVER]: fakeServer });
  assert.ok(on.systemPrompt.append.includes(RECALL_PROMPT_APPEND));
  // Read-only, so subagents may recall too: no gate is registered, and no allowedTools entry shadows canUseTool.
  // Only the exec policy's Bash hook (#471), which every colony gets; recall adds no gate of its own.
  assert.deepEqual((on.hooks?.PreToolUse ?? []).map((e) => e.matcher), ['Bash']);
  assert.equal(on.allowedTools, undefined);
});

test('recall sits beside the other colony servers rather than replacing them', () => {
  const { options } = buildOptions(
    { ...recallEnv, COLONIZER_FINDINGS: 'true', COLONIZER_MEMORY_DIR: '/colonizer/memory' },
    { recallServer: fakeServer, findingsServer: { name: 'colonizer_findings' }, memoryServer: { name: 'colonizer_memory' } },
  );
  assert.deepEqual(Object.keys(options.mcpServers).sort(), ['colonizer_findings', 'colonizer_memory', 'colonizer_recall']);
});
