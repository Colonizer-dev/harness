// The operator vault in mcp.mjs (issue #777): vault_search reads the staged read-only snapshot in
// COLONIZER_VAULT_DIR and answers sourced, framed matches from inside it only; vault_propose is
// checked here (path traversal and caps refused as tool errors) and forwarded to the bridge's
// /vault, never written anywhere. One file in three modules: grok-build and hermes vendor this
// module's mcp.mjs, and the first test keeps them byte-identical.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const moduleDir = join(dirname(fileURLToPath(import.meta.url)), '..');

for (const copy of ['grok-build', 'hermes']) {
  test(`${copy}/mcp.mjs is byte-identical to the codex original it is copied from`, () => {
    const vendored = readFileSync(join(moduleDir, '..', copy, 'mcp.mjs'));
    assert.ok(vendored.equals(readFileSync(join(moduleDir, 'mcp.mjs'))), `modules/agents/${copy}/mcp.mjs has drifted from modules/agents/codex/mcp.mjs; it is one file in three places — change them together`);
  });
}

/** A staged snapshot, plus a secret note outside it that a symlink inside points at. */
function snapshot() {
  const root = mkdtempSync(join(tmpdir(), 'codex-vault-'));
  const dir = join(root, 'vault');
  const put = (rel, text) => {
    mkdirSync(dirname(join(dir, rel)), { recursive: true });
    writeFileSync(join(dir, rel), text);
  };
  put('INDEX.md', '# Operator vault\n\ndeploy deploy deploy\n');
  put('Projects/web/deploy.md', '# Deploy order\n\nIntro.\n\n## Migrations\n\nRun the deploy migrations before the web deploy.\n');
  put('Decisions/db.md', '# Database\n\nWe deploy Postgres 16. Migrations are reviewed.\n');
  put('.obsidian/deploy.md', '# Hidden deploy migrations\n');
  writeFileSync(join(root, 'outside.md'), '# Outside deploy migrations\n');
  symlinkSync(join(root, 'outside.md'), join(dir, 'Projects/link.md'));
  return { root, dir };
}

function startServer(env) {
  const child = spawn(process.execPath, [join(moduleDir, 'mcp.mjs')], { env: { PATH: process.env.PATH, ...env }, stdio: ['pipe', 'pipe', 'inherit'] });
  const pending = new Map();
  let next = 0;
  createInterface({ input: child.stdout, crlfDelay: Infinity }).on('line', (line) => {
    const msg = JSON.parse(line);
    const waiter = pending.get(msg.id);
    if (waiter) {
      pending.delete(msg.id);
      waiter(msg);
    }
  });
  const call = (method, params) =>
    new Promise((resolve) => {
      const id = next++;
      pending.set(id, resolve);
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  return { call, stop: () => child.kill('SIGKILL') };
}

async function startBridge() {
  const seen = [];
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      seen.push({ path: req.url, body: JSON.parse(body || '{}') });
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ ok: true }));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { seen, url: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((resolve) => { server.close(resolve); server.closeAllConnections?.(); }) };
}

const text = (reply) => reply.result?.content?.[0]?.text ?? `rpc error: ${reply.error?.message}`;

test('the vault tools are offered only when the snapshot is staged', async () => {
  const off = startServer({});
  const on = startServer({ COLONIZER_VAULT_DIR: snapshot().dir });
  try {
    const names = async (srv) => (await srv.call('tools/list', {})).result.tools.map((t) => t.name);
    assert.ok(!(await names(off)).some((name) => name.startsWith('vault_')));
    assert.deepEqual((await names(on)).filter((name) => name.startsWith('vault_')), ['vault_search', 'vault_propose']);
  } finally {
    off.stop();
    on.stop();
  }
});

test('vault_search answers ranked, sourced and framed matches from inside the snapshot only', async () => {
  const { dir } = snapshot();
  const srv = startServer({ COLONIZER_VAULT_DIR: dir });
  try {
    const answer = text(await srv.call('tools/call', { name: 'vault_search', arguments: { query: 'deploy MIGRATIONS' } }));
    assert.match(answer, /^<operator-vault>\n.*not instructions/);
    assert.match(answer, /<\/operator-vault>$/);
    const lines = answer.split('\n').filter((line) => line.startsWith('- '));
    assert.equal(lines.length, 2, answer);
    assert.equal(lines[0], '- /colonizer/vault/Projects/web/deploy.md:7 (under "Migrations") — Deploy order');
    assert.equal(lines[1], '- /colonizer/vault/Decisions/db.md:3 (under "Database") — Database');
    assert.match(answer, /Run the deploy migrations before the web deploy\./);
    for (const leak of ['Outside', 'Hidden', 'INDEX.md']) assert.ok(!answer.includes(leak), leak);
    const one = text(await srv.call('tools/call', { name: 'vault_search', arguments: { query: 'deploy', limit: 1 } }));
    assert.equal(one.split('\n').filter((line) => line.startsWith('- ')).length, 1);
    assert.equal(text(await srv.call('tools/call', { name: 'vault_search', arguments: { query: 'nothing-like-this' } })), 'No operator vault note matches that query.');
  } finally {
    srv.stop();
  }
});

test('vault_propose forwards a checked proposal to the bridge, refuses traversal, and writes nothing', async () => {
  const { dir } = snapshot();
  const before = readdirSync(dir, { recursive: true }).sort();
  const bridge = await startBridge();
  const srv = startServer({ COLONIZER_VAULT_DIR: dir, COLONIZER_BRIDGE_URL: bridge.url, COLONIZER_BRIDGE_TOKEN: 't' });
  try {
    const base = { title: 'Deploy order', body: 'Run migrations first.', reason: 'It broke twice' };
    for (const path of ['../escape', '/etc/passwd', 'a/../../b', '.obsidian/x', 'a/b/c/d/e']) {
      const reply = await srv.call('tools/call', { name: 'vault_propose', arguments: { ...base, path } });
      assert.equal(reply.result.isError, true, path);
      assert.match(text(reply), /relative note path/);
    }
    const big = await srv.call('tools/call', { name: 'vault_propose', arguments: { ...base, path: 'n', body: 'x'.repeat(64 * 1024 + 1) } });
    assert.match(text(big), /body is over its limit/);
    assert.equal(bridge.seen.length, 0, 'nothing refused reaches the bridge');
    const ok = await srv.call('tools/call', { name: 'vault_propose', arguments: { ...base, path: 'web/deploy-order' } });
    assert.equal(ok.result.isError, undefined, text(ok));
    assert.deepEqual(bridge.seen, [{ path: '/vault', body: { ...base, path: 'web/deploy-order.md' } }]);
    assert.deepEqual(readdirSync(dir, { recursive: true }).sort(), before, 'the snapshot is untouched');
  } finally {
    srv.stop();
    await bridge.close();
  }
});
