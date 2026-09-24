import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { AsyncQueue, MAX_TOOL_OUTPUT, archPlatform, createBridge, defaultCacheDir, mapLine, opencodeConfig, parseRoutes, preflight, resolveOpencode, runAgent, splitModel } from '../runner.mjs';

const ROUTES = parseRoutes(JSON.stringify([
  { provider: 'local', prefix: 'local/', base_url: 'http://gw:41750/providers/local', auth: 'none', headers: { 'x-colonizer-colony': 'tok' }, timeout_secs: 900, context_tokens: 131072 },
  { provider: 'o', prefix: 'o/', base_url: 'http://gw:41750/providers/o', auth: 'none', headers: {} },
])).routes;
const J = (o) => JSON.stringify(o);
const ALL = []; // every emitted event, for the contract check at the end
const FINISH = { type: 'step_finish', sessionID: 'ses_1', part: { reason: 'stop', tokens: { total: 30, input: 20, output: 8, reasoning: 2, cache: { read: 100, write: 50 } }, cost: 0.001 } };

/** A fake opencode child: emits `run.lines` on spawn, records args/stdin/kills. */
function fakeSpawn(runs) {
  const spawns = [];
  const impl = (bin, args, opts) => {
    const run = runs[Math.min(spawns.length, runs.length - 1)] ?? {};
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stderr = new EventEmitter();
    child.stdin = { data: '', write(c) { this.data += c; }, end() {} };
    child.kills = [];
    let finished = false;
    const finish = (code) => { if (!finished) { finished = true; child.emit('exit', code); } };
    child.kill = (sig) => { child.kills.push(sig); if (sig === 'SIGINT' && !run.stay) finish(130); if (sig === 'SIGKILL') finish(137); return true; };
    const rec = { bin, args, env: opts.env, stdin: child.stdin, kills: child.kills, finish, emitLines: (lines) => child.stdout.emit('data', `${lines.map((l) => (typeof l === 'string' ? l : J(l))).join('\n')}\n`), raw: (c) => child.stdout.emit('data', c), err: (d) => child.stderr.emit('data', d) };
    spawns.push(rec);
    queueMicrotask(() => { if (run.hang) return; rec.emitLines(run.lines ?? []); if (!run.noAuto) finish(run.code ?? 0); });
    return child;
  };
  impl.spawns = spawns;
  return impl;
}

const waitFor = async (events, pred, what) => {
  const start = Date.now();
  while (Date.now() - start < 10_000) {
    const hit = events.find(pred);
    if (hit) return hit;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error(`timed out waiting for ${what}`);
};

function drive(agentArgs = {}) {
  const events = [];
  const commands = new AsyncQueue();
  const extra = agentArgs.onReady;
  const done = runAgent({ commands, emit: (e) => { events.push(e); ALL.push(e); }, bin: 'opencode', model: 'local/fake', routes: ROUTES, makeEnv: () => ({}), firstOutputMs: 1000, graceMs: 50, ...agentArgs, onReady: (b) => { drive.bridge = b; extra?.(b); } });
  return { events, commands, done, end: async () => { commands.push({ type: 'shutdown' }); await done.catch(() => {}); } };
}

const post = (bridge, path, body, token) => fetch(`${bridge.url}${path}`, { method: 'POST', headers: { authorization: `Bearer ${token ?? bridge.token}`, 'content-type': 'application/json' }, body: J(body) });

test('config maps a local auth:none route onto the gateway', () => {
  const cfg = opencodeConfig({ routes: ROUTES, model: 'local/fake', smallModel: '' });
  const p = cfg.provider.local;
  assert.equal(p.npm, '@ai-sdk/anthropic');
  assert.equal(p.options.baseURL, 'http://gw:41750/providers/local/v1');
  assert.equal(p.options.apiKey, 'colonizer');
  assert.equal(p.options.headers['x-colonizer-colony'], 'tok');
  assert.equal(p.options.timeout, 900_000);
  assert.deepEqual(p.models.fake.limit, { context: 131072, output: 32000 });
  assert.equal(cfg.small_model, 'local/fake');
  assert.equal(cfg.permission, 'allow');
  assert.equal(parseRoutes('nope').warnings.length, 1);
  assert.equal(archPlatform({ arch: 'x64', cpuinfo: 'flags : avx2' }), 'linux-x64');
  assert.equal(archPlatform({ arch: 'x64', cpuinfo: 'flags : sse' }), 'linux-x64-baseline');
});

test('preflight names the fix for every unrouted model', () => {
  for (const bad of ['', 'deepseek-flash', 'local/', 'x/y']) assert.match(preflight(bad, ROUTES), /Settings → Providers/);
  assert.equal(preflight('local/fake', ROUTES), null);
  assert.equal(splitModel('local/fake', ROUTES).name, 'fake');
});

test('mapLine maps text, thinking, tools, usage and errors', () => {
  const st = { model: 'local/fake', usage: {}, sessionId: null, lastText: null, failed: null, block: 0, msg: 1 };
  assert.deepEqual(mapLine({ type: 'text', sessionID: 'ses_9', part: { text: 'hi', messageID: 'msg_1' } }, st), [{ type: 'assistant_text', message_id: 'msg_1', block_index: 0, text: 'hi' }]);
  assert.equal(st.sessionId, 'ses_9');
  assert.deepEqual(mapLine({ type: 'reasoning', part: { text: 'hmm' } }, st), [{ type: 'thinking', message_id: 'm-1', block_index: 1, text: 'hmm' }]);
  const [call, result] = mapLine({ type: 'tool_use', part: { tool: 'bash', callID: 'toolu_1', messageID: 'msg_2', state: { status: 'completed', input: { command: 'ls' }, output: 'x' } } }, st);
  assert.equal(call.type, 'tool_call');
  assert.deepEqual(call.input, { command: 'ls' });
  assert.deepEqual(result, { type: 'tool_result', tool_call_id: 'toolu_1', output: 'x', is_error: false });
  assert.deepEqual(mapLine({ type: 'tool_use', part: { tool: 'colonizer_ask_user', callID: 'c', state: { status: 'completed', input: {}, output: '{}' } } }, st), []);
  assert.deepEqual(mapLine(FINISH, st), []);
  assert.deepEqual(st.usage['local/fake'], { input_tokens: 20, output_tokens: 8, cache_read_tokens: 100, cache_write_tokens: 50 });
  const big = mapLine({ type: 'tool_use', part: { tool: 'bash', callID: 'c2', state: { status: 'completed', input: {}, output: 'y'.repeat(MAX_TOOL_OUTPUT + 10) } } }, st)[1];
  assert.ok(big.output.length <= MAX_TOOL_OUTPUT && big.output.includes('truncated'));
  assert.deepEqual(mapLine({ type: 'error', error: { name: 'X', data: { message: 'boom' } } }, st), [{ type: 'log', level: 'error', message: 'boom' }]);
  assert.equal(st.failed, 'boom');
  const st2 = { model: 'local/fake', usage: {}, sessionId: null, lastText: null, failed: null, block: 0, msg: 1 };
  assert.deepEqual(mapLine({ type: 'error', error: { message: 'provider says no' } }, st2), [{ type: 'log', level: 'error', message: 'provider says no' }]);
});

test('two turns: echo, text, tool, usage, turn_end, --session, exited', async () => {
  const spawnImpl = fakeSpawn([
    { lines: [{ type: 'step_start', sessionID: 'ses_1' }, { type: 'text', sessionID: 'ses_1', part: { text: 'done one', messageID: 'm1' } }, { type: 'reasoning', sessionID: 'ses_1', part: { text: 'plan' } }, { type: 'tool_use', sessionID: 'ses_1', part: { tool: 'bash', callID: 'toolu_1', messageID: 'm1', state: { status: 'completed', input: { command: 'ls' }, output: 'x' } } }, FINISH] },
    { lines: [{ type: 'text', sessionID: 'ses_1', part: { text: 'done two' } }, FINISH] },
  ]);
  const t = drive({ spawnImpl });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'first' });
    const end1 = await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end 1');
    assert.equal(end1.is_error, false);
    assert.equal(end1.result, 'done one');
    assert.equal(end1.cost_usd, 0);
    assert.equal(typeof end1.duration_ms, 'number');
    assert.deepEqual(end1.model_usage['local/fake'], { input_tokens: 20, output_tokens: 8, cache_read_tokens: 100, cache_write_tokens: 50 });
    assert.ok(t.events.some((e) => e.type === 'user_message' && e.id === 'u-1'));
    assert.ok(t.events.some((e) => e.type === 'assistant_text' && e.text === 'done one'));
    assert.ok(t.events.some((e) => e.type === 'thinking' && e.text === 'plan'));
    assert.ok(!spawnImpl.spawns[0].args.includes('--session'));
    t.commands.push({ type: 'user_message', id: 'u-2', text: 'second' });
    const end2 = await waitFor(t.events, (e) => e.type === 'turn_end' && e.result === 'done two', 'turn_end 2');
    assert.equal(end2.model_usage['local/fake'].input_tokens, 40); // cumulative across turns
    const args2 = spawnImpl.spawns[1].args;
    assert.ok(args2.includes('--session') && args2.includes('ses_1'));
    assert.equal(spawnImpl.spawns[1].stdin.data, 'second');
  } finally {
    await t.end();
  }
  assert.deepEqual(t.events.at(-1), { type: 'status', state: 'exited' });
});

test('interrupt sends SIGINT and is not an error', async () => {
  const spawnImpl = fakeSpawn([{ lines: [{ type: 'text', sessionID: 's', part: { text: 'ok' } }] }, { hang: true }]);
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'one' });
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
    t.commands.push({ type: 'user_message', id: 'u-2', text: 'two' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    t.commands.push({ type: 'interrupt' });
    await waitFor(t.events, () => spawnImpl.spawns[1].kills.includes('SIGINT'), 'SIGINT');
    await waitFor(t.events, (e) => e.type === 'turn_end' && t.events.filter((x) => x.type === 'turn_end').length === 2, 'interrupted turn_end');
    assert.equal(t.events.filter((e) => e.type === 'turn_end').at(-1).is_error, false);
  } finally {
    await t.end();
  }
});

test('a JSON line split across pipe chunks still parses', async () => {
  const spawnImpl = fakeSpawn([{ hang: true }]);
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'hi' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    const line = J({ type: 'text', sessionID: 's', part: { text: 'split me' } });
    spawnImpl.spawns[0].raw(`${line.slice(0, 20)}`);
    spawnImpl.spawns[0].raw(`${line.slice(20)}\n`);
    const got = await waitFor(t.events, (e) => e.type === 'assistant_text', 'split text');
    assert.equal(got.text, 'split me');
    spawnImpl.spawns[0].finish(0);
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
  } finally {
    await t.end();
  }
});

test('answering one of two asks stays waiting until both resolve', async () => {
  const spawnImpl = fakeSpawn([{ hang: true }]);
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'ask twice' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    const ask1 = post(drive.bridge, '/ask', { questions: [] });
    const q1 = await waitFor(t.events, (e) => e.type === 'question', 'q1');
    const ask2 = post(drive.bridge, '/ask', { questions: [] });
    const q2 = await waitFor(t.events, (e) => e.type === 'question' && e.question_id !== q1.question_id, 'q2');
    t.commands.push({ type: 'answer', question_id: q1.question_id, answers: {} });
    await waitFor(t.events, (e) => e.type === 'question_answered' && e.question_id === q1.question_id, 'a1');
    await ask1;
    const afterA1 = t.events.slice(t.events.findIndex((e) => e.type === 'question_answered'));
    assert.ok(!afterA1.some((e) => e.type === 'status' && e.state === 'working')); // still one ask open
    t.commands.push({ type: 'answer', question_id: q2.question_id, answers: {} });
    await waitFor(t.events, (e) => e.type === 'question_answered' && e.question_id === q2.question_id, 'a2');
    await ask2;
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working again');
    spawnImpl.spawns[0].emitLines([{ type: 'text', sessionID: 's', part: { text: 'done' } }]);
    spawnImpl.spawns[0].finish(0);
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
  } finally {
    await t.end();
  }
});

test('interrupt with a pending ask cancels it; a late answer warns', async () => {
  const spawnImpl = fakeSpawn([{ hang: true }]);
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'ask me' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    const asked = post(drive.bridge, '/ask', { questions: [] }).then((r) => r.json());
    const q = await waitFor(t.events, (e) => e.type === 'question', 'question');
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'waiting_for_answer', 'waiting');
    t.commands.push({ type: 'interrupt' });
    assert.deepEqual(await asked, { cancelled: true }); // the dead turn's asker is released
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
    assert.ok(!t.events.some((e) => e.type === 'question_answered')); // claude-code emits none either
    const afterEnd = t.events.slice(t.events.findIndex((e) => e.type === 'turn_end'));
    assert.ok(afterEnd.some((e) => e.type === 'status' && e.state === 'idle'));
    assert.ok(!afterEnd.some((e) => e.type === 'status' && e.state === 'waiting_for_answer'));
    t.commands.push({ type: 'answer', question_id: q.question_id, answers: {} });
    await waitFor(t.events, (e) => e.type === 'log' && e.message.startsWith('no open question'), 'late-answer warn');
  } finally {
    await t.end();
  }
});

test('stderr is redacted of the gateway token', async () => {
  const spawnImpl = fakeSpawn([{ hang: true }]);
  const chunks = [];
  const orig = process.stderr.write.bind(process.stderr);
  process.stderr.write = (c) => { chunks.push(String(c)); return true; };
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'hi' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    spawnImpl.spawns[0].err('config header x-colonizer-colony: tok leaked\n');
    await new Promise((r) => setTimeout(r, 50));
    assert.ok(chunks.join('').includes('[redacted]'));
    assert.ok(!chunks.join('').includes(': tok '));
    spawnImpl.spawns[0].emitLines([{ type: 'text', sessionID: 's', part: { text: 'done' } }]);
    spawnImpl.spawns[0].finish(0);
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
  } finally {
    process.stderr.write = orig;
    await t.end();
  }
});

test('question round-trip through the bridge', async () => {
  const spawnImpl = fakeSpawn([{ hang: true }]);
  const t = drive({ spawnImpl, firstOutputMs: 30_000 });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'ask me' });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'working');
    const asked = post(drive.bridge, '/ask', { questions: [{ question: 'Pick?', header: 'P', multiSelect: false, options: [{ label: 'A', description: 'first' }, { label: 'B', description: 'second' }] }] }).then((r) => r.json());
    const q = await waitFor(t.events, (e) => e.type === 'question', 'question');
    assert.deepEqual(q.questions, [{ question: 'Pick?', header: 'P', multi_select: false, options: [{ label: 'A', description: 'first', preview: null }, { label: 'B', description: 'second', preview: null }] }]);
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'waiting_for_answer', 'waiting');
    t.commands.push({ type: 'answer', question_id: q.question_id, answers: { 'Pick?': 'A' } });
    const answered = await waitFor(t.events, (e) => e.type === 'question_answered', 'answered');
    assert.deepEqual(answered.answers, { 'Pick?': 'A' });
    assert.equal(answered.response, null);
    assert.deepEqual(await asked, { answers: { 'Pick?': 'A' }, response: null });
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'working', 'back to working');
    spawnImpl.spawns[0].emitLines([{ type: 'text', sessionID: 's', part: { text: 'there' } }]);
    spawnImpl.spawns[0].finish(0);
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
  } finally {
    await t.end();
  }
});

test('set_model switches the next turn, rejects the rest', async () => {
  const spawnImpl = fakeSpawn([{ lines: [{ type: 'text', sessionID: 's', part: { text: 'x' } }] }]);
  const seenModels = [];
  const t = drive({ spawnImpl, makeEnv: (b, m) => { seenModels.push(m); return {}; } });
  try {
    await waitFor(t.events, (e) => e.type === 'status' && e.state === 'idle', 'idle');
    t.commands.push({ type: 'set_model', model: 'o/m2' });
    t.commands.push({ type: 'set_model', model: 'bare' });
    t.commands.push({ type: 'set_model', model: 'x/y' });
    t.commands.push({ type: 'user_message', id: 'u-1', text: 'go' });
    await waitFor(t.events, (e) => e.type === 'turn_end', 'turn_end');
    assert.deepEqual(t.events.find((e) => e.type === 'model_changed'), { type: 'model_changed', model: 'o/m2', previous: 'local/fake' });
    assert.equal(t.events.filter((e) => e.type === 'log' && e.level === 'warn' && e.message.startsWith('ignored set_model')).length, 2);
    assert.ok(spawnImpl.spawns[0].args.includes('o/m2'));
    assert.deepEqual(seenModels, ['o/m2']); // the turn's config is rebuilt for the new model
  } finally {
    await t.end();
  }
});

test('resolveOpencode prefers env and PATH, refuses a bad sha256', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-oc-test-'));
  const fakeBytes = Buffer.from('fake-tgz-bytes');
  const { createHash } = await import('node:crypto');
  const good = createHash('sha256').update(fakeBytes).digest('hex');
  const lock = (sha) => `opencode  1.18.32  linux-arm64  agent  ${sha}  https://example.invalid/t.tgz`;
  const fetchImpl = async () => ({ ok: true, arrayBuffer: async () => fakeBytes });
  let tarCalled = 0;
  const runTar = async (args) => { tarCalled++; const dest = args[args.indexOf('-C') + 1]; mkdirSync(join(dest, 'package', 'bin'), { recursive: true }); writeFileSync(join(dest, 'package', 'bin', 'opencode'), '#!/bin/sh\n'); };
  assert.equal(await resolveOpencode({ env: { COLONIZER_OPENCODE_BIN: '/custom/opencode' }, lockText: '' }), '/custom/opencode');
  mkdirSync(join(dir, 'pathdir'), { recursive: true });
  writeFileSync(join(dir, 'pathdir', 'opencode'), 'x');
  assert.equal(await resolveOpencode({ env: { PATH: join(dir, 'pathdir') }, lockText: '', arch: 'arm64', cpuinfo: '' }), join(dir, 'pathdir', 'opencode'));
  await assert.rejects(resolveOpencode({ env: {}, lockText: lock('0'.repeat(64)), arch: 'arm64', cpuinfo: '', fetchImpl, runTar, cacheDir: join(dir, 'bad') }), /sha256 mismatch/);
  assert.equal(tarCalled, 0);
  const bin = await resolveOpencode({ env: {}, lockText: lock(good), arch: 'arm64', cpuinfo: '', fetchImpl, runTar, cacheDir: join(dir, 'good'), log: () => {} });
  assert.ok(bin.endsWith('/opencode'));
  assert.equal(defaultCacheDir({ XDG_CACHE_HOME: '/x' }), '/x/colonizer/opencode');
  assert.equal(defaultCacheDir({ HOME: '/h' }), '/h/.cache/colonizer/opencode');
  assert.ok(defaultCacheDir({ PATH: '/bin' }).endsWith('colonizer-opencode'));
  await assert.rejects(resolveOpencode({ env: {}, lockText: lock(good), arch: 'arm64', cpuinfo: '', fetchImpl, runTar: async () => { throw new Error('tar blew up'); }, cacheDir: join(dir, 'fail') }), /tar blew up/);
  assert.ok(!existsSync(join(dir, 'fail', '1.18.32', 'linux-arm64', 'pkg.tgz'))); // no 60 MB left behind
});

test('bridge files findings and proposes memory, and needs its token', async () => {
  const events = [];
  let state = 'idle';
  const b = await createBridge({ emit: (e) => { events.push(e); ALL.push(e); }, setStatus: (s) => { state = s; }, isWorking: () => false, findings: true, token: 't' });
  try {
    assert.equal((await (await post(b, '/finding', { title: 'T', body: 'B', evidence: 'E' })).json()).filed, true);
    assert.deepEqual(events.at(-1), { type: 'finding', title: 'T', body: 'B', evidence: 'E' });
    assert.equal((await (await post(b, '/memory', { scope: 'org', title: 'M', content: 'C', tags: ['a'] })).json()).ok, true);
    assert.deepEqual(events.at(-1), { type: 'memory_proposal', scope: 'org', title: 'M', content: 'C', tags: ['a'] });
    assert.match((await (await post(b, '/memory', { scope: 'bogus', title: 'M', content: 'C' })).json()).error, /repo, org or global/);
    assert.match((await (await post(b, '/finding', { title: 'T', body: '', evidence: '' })).json()).error, /body, evidence/);
    assert.equal(events.filter((e) => e.type === 'finding').length, 1); // rejected calls emit nothing
    assert.equal(await post(b, '/ask', { questions: [] }, 'wrong').then((r) => r.status), 401);
    assert.equal(state, 'idle');
  } finally {
    await b.close();
  }
});

test('every emitted event carries its schema-required fields', () => {
  const schema = JSON.parse(readFileSync(new URL('../../../../docs/agent-events.schema.json', import.meta.url), 'utf8'));
  const required = new Map();
  for (const def of Object.values(schema.$defs)) if (def.properties?.type?.const && Array.isArray(def.required)) required.set(def.properties.type.const, def.required);
  assert.ok(ALL.length > 20);
  for (const e of ALL) {
    const keys = required.get(e.type);
    assert.ok(keys, `unknown event type ${e.type}`);
    for (const k of keys) assert.ok(e[k] !== undefined, `${e.type} missing ${k}: ${J(e)}`);
  }
  const types = new Set(ALL.map((e) => e.type));
  for (const t of ['status', 'user_message', 'assistant_text', 'thinking', 'tool_call', 'tool_result', 'question', 'question_answered', 'turn_end', 'log', 'model_changed', 'finding', 'memory_proposal']) assert.ok(types.has(t), `no ${t} event emitted`);
});
