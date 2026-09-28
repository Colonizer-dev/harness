// Direct tests for mcp.mjs, the colonizer MCP stdio server: the tool list's gating, memory_search
// against a mounted notes dir, and wait's refusals, match and timeout-with-tail. The end-to-end path
// (bridge forwarding, the runner's -c registration) is covered in runner.test.mjs.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { appendFileSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';

function startServer(env) {
  // A minimal env on purpose: a colony shell can carry COLONIZER_FINDINGS/COLONIZER_MEMORY_DIR.
  const child = spawn(process.execPath, [new URL('../mcp.mjs', import.meta.url).pathname], { env, stdio: ['pipe', 'pipe', 'inherit'] });
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
  return { child, call, stop: () => child.kill('SIGKILL') };
}

const resultText = (reply) => reply.result?.content?.[0]?.text ?? `rpc error: ${reply.error?.message ?? 'none'}`;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test('the tool list follows the switches, and memory_search reads the mounted notes', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'codex-mcp-mem-'));
  mkdirSync(join(dir, 'repo', 'notes'), { recursive: true });
  writeFileSync(join(dir, 'repo', 'notes', 'waiting.md'), '# Wait rooms\nThe waitrooms convention: call wait instead of polling.\n');
  const srv = startServer({ COLONIZER_MEMORY_DIR: dir });
  try {
    assert.equal((await srv.call('initialize', {})).result.protocolVersion, '2024-11-05');
    assert.deepEqual((await srv.call('tools/list', {})).result.tools.map((t) => t.name), ['memory_search', 'memory_propose', 'wait']);
    const hit = await srv.call('tools/call', { name: 'memory_search', arguments: { query: 'waitrooms' } });
    assert.equal(hit.result.isError, undefined);
    assert.match(resultText(hit), /\[repo\] Wait rooms \(/);
    assert.match(resultText(hit), /waitrooms convention/);
    const miss = await srv.call('tools/call', { name: 'memory_search', arguments: { query: 'nothing matches this' } });
    assert.equal(resultText(miss), 'No shared memory matches that query.');
    const unknown = await srv.call('tools/call', { name: 'nope', arguments: {} });
    assert.equal(unknown.error.code, -32602);
  } finally {
    srv.stop();
  }
});

test('wait refuses malformed calls as tool errors, never as rpc failures', async () => {
  const srv = startServer({});
  const call = (arguments_) => srv.call('tools/call', { name: 'wait', arguments: arguments_ });
  try {
    assert.match(resultText(await call({})), /reason is required/);
    assert.match(resultText(await call({ reason: 'r' })), /nothing to wait for/);
    assert.match(resultText(await call({ reason: 'r', seconds: 1, file: '/tmp/x' })), /alternatives/);
    assert.match(resultText(await call({ reason: 'r', pattern: 'x' })), /file and pattern go together/);
    assert.match(resultText(await call({ reason: 'r', seconds: -5 })), /non-negative/);
    assert.match(resultText(await call({ reason: 'r', pid: 0 })), /process id/);
    assert.match(resultText(await call({ reason: 'r', file: '/tmp/x', pattern: '(' })), /not a valid JavaScript regular expression/);
  } finally {
    srv.stop();
  }
});

test('wait returns on a matching line, and its timeout carries the file tail', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'codex-mcp-wait-'));
  const log = join(dir, 'build.log');
  writeFileSync(log, 'step one ok\nstep two started\n');
  const srv = startServer({});
  const wait = (arguments_) => srv.call('tools/call', { name: 'wait', arguments: arguments_ });
  try {
    const hit = await wait({ reason: 'build', file: log, pattern: 'step two' });
    assert.match(resultText(hit), /^Matched \/step two\/ in .*build\.log after .* \(build\)\.\nMatching line: step two started/);
    const miss = await wait({ reason: 'build', file: log, pattern: 'never appears', timeout_seconds: 1 });
    assert.match(resultText(miss), /^Timed out after 1(\.\d)? s .* \(build\)\.\nLast lines of .*build\.log:\nstep one ok\nstep two started/);
    const absent = await wait({ reason: 'gone', file: join(dir, 'missing.log'), pattern: 'x', timeout_seconds: 1 });
    assert.match(resultText(absent), /The file never appeared\./);
    const nap = await wait({ reason: 'pause', seconds: 0 });
    assert.match(resultText(nap), /^Waited .* \(pause\)\.$/);
  } finally {
    srv.stop();
  }
});

test('a growing last line is matched from its own bytes only, and non-regular files are refused', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'codex-mcp-grow-'));
  const log = join(dir, 'build.log');
  writeFileSync(log, '12');
  const srv = startServer({});
  const wait = (arguments_) => srv.call('tools/call', { name: 'wait', arguments: arguments_ });
  try {
    const growing = wait({ reason: 'grow', file: log, pattern: '2123', timeout_seconds: 1 });
    await sleep(300);
    appendFileSync(log, '3456\n'); // the line becomes 123456 — a prefix-carrying re-read would see 12123456
    assert.match(resultText(await growing), /^Timed out after 1(\.\d)? s .* \(grow\)\.\nLast lines of .*build\.log/);
    assert.match(resultText(await wait({ reason: 'dir', file: dir, pattern: 'x' })), /^Could not wait: .* is a directory; only a regular file can be watched\.$/);
  } finally {
    srv.stop();
  }
});
