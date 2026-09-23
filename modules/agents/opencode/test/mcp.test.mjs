import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { test } from 'node:test';

/** Stub bridge: canned answers per path. */
async function stubBridge(answers) {
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      let parsed = {};
      try { parsed = JSON.parse(body || '{}'); } catch {}
      const a = answers[req.url];
      const out = typeof a === 'function' ? a(parsed) : (a ?? {});
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(out));
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { url: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((r) => { server.close(r); server.closeAllConnections?.(); }) };
}

function startServer(env) {
  const child = spawn(process.execPath, [new URL('../mcp.mjs', import.meta.url).pathname], { env: { ...process.env, ...env }, stdio: ['pipe', 'pipe', 'inherit'] });
  let buf = '';
  const pending = new Map();
  let next = 1;
  child.stdout.on('data', (c) => {
    buf += c;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      const msg = JSON.parse(line);
      if (msg.method === 'notifications/progress') continue;
      const w = pending.get(msg.id);
      if (w) { pending.delete(msg.id); w(msg); }
    }
  });
  const call = (method, params) => new Promise((resolve) => { const id = next++; pending.set(id, resolve); child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`); });
  return { child, call, stop: () => child.kill('SIGKILL') };
}

test('mcp lists tools and forwards calls to the bridge', async () => {
  const bridge = await stubBridge({ '/ask': { answers: { Color: 'Blue' }, response: null }, '/finding': { filed: true }, '/memory': (b) => (['repo', 'org', 'global'].includes(b.scope) ? { ok: true } : { error: 'memory_propose scope must be repo, org or global' }) });
  const srv = startServer({ COLONIZER_BRIDGE_URL: bridge.url, COLONIZER_BRIDGE_TOKEN: 'tok' });
  try {
    assert.equal((await srv.call('initialize', {})).result.protocolVersion, '2024-11-05');
    const names = (await srv.call('tools/list', {})).result.tools.map((t) => t.name);
    assert.deepEqual(names, ['ask_user', 'finding_file', 'memory_propose']);
    const ask = await srv.call('tools/call', { name: 'ask_user', arguments: { questions: [{ question: 'Color?', options: [{ label: 'Blue' }] }] }, _meta: { progressToken: 7 } });
    assert.deepEqual(JSON.parse(ask.result.content[0].text), { answers: { Color: 'Blue' }, response: null });
    const filing = await srv.call('tools/call', { name: 'finding_file', arguments: { title: 'T', body: 'B', evidence: 'E' } });
    assert.equal(JSON.parse(filing.result.content[0].text).filed, true);
    const rejected = await srv.call('tools/call', { name: 'memory_propose', arguments: { scope: 'bogus', title: 'M', content: 'C' } });
    assert.equal(rejected.result.isError, true);
    assert.match(rejected.result.content[0].text, /repo, org or global/);
    const bad = await srv.call('tools/call', { name: 'nope', arguments: {} });
    assert.equal(bad.error.code, -32602);
  } finally {
    srv.stop();
    await bridge.close();
  }
});
