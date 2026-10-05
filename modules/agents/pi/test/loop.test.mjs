// The loop tools in the Pi module (issue #643): loop-extension.mjs registers loop_stop for a loop
// colony, plus loop_next when the loop is self-paced; the runner loads it by explicit path and
// starts a loopback loop bridge whose coordinates ride in Pi's env, and each call leaves the colony
// as a `loop_next` or `loop_stop` protocol event. The last test drives the real Pi (RPC mode)
// against a stand-in Anthropic endpoint.

import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import colonizerLoop, { piLoopTools } from '../loop-extension.mjs';
import { createLoopBridge } from '../loop-tools.mjs';
import { buildModelsConfig, commandQueue, LOOP_EXTENSION, parseRoutes, piArgs, piEnv, runAgent } from '../runner.mjs';

const moduleDir = join(dirname(fileURLToPath(import.meta.url)), '..');

test('pi/loop-tools.mjs is byte-identical to the acp original it is copied from', () => {
  const copy = readFileSync(join(moduleDir, 'loop-tools.mjs'));
  const original = readFileSync(join(moduleDir, '..', 'acp', 'loop-tools.mjs'));
  assert.ok(copy.equals(original), 'modules/agents/pi/loop-tools.mjs has drifted from modules/agents/acp/loop-tools.mjs; it is one file in two places — change them together');
});

test('the manifest declares loop_tools, so a self-paced loop on Pi is briefed with loop_next instead of falling back to every 24 hours', () => {
  assert.equal(JSON.parse(readFileSync(join(moduleDir, 'module.json'), 'utf8')).loop_tools, true);
});

test('the extension registers loop_stop for a loop colony, loop_next only when it is self-paced, and nothing outside a loop', (t) => {
  const saved = { ...process.env };
  t.after(() => {
    for (const key of ['COLONIZER_LOOP', 'COLONIZER_LOOP_SELF_PACED']) (saved[key] === undefined ? delete process.env[key] : (process.env[key] = saved[key]));
  });
  const names = () => {
    const registered = [];
    colonizerLoop({ registerTool: (tool) => registered.push(tool) });
    return registered.map((tool) => tool.name);
  };
  delete process.env.COLONIZER_LOOP;
  delete process.env.COLONIZER_LOOP_SELF_PACED;
  assert.deepEqual(names(), [], 'not a loop colony, no tools');
  process.env.COLONIZER_LOOP = 'true';
  process.env.COLONIZER_LOOP_SELF_PACED = 'false';
  assert.deepEqual(names(), ['loop_stop']);
  process.env.COLONIZER_LOOP_SELF_PACED = 'true';
  assert.deepEqual(names(), ['loop_next', 'loop_stop']);
});

test('the tools cross the loop bridge: a clamped loop_next and a loop_stop become protocol events; a refusal emits nothing', async (t) => {
  const events = [];
  const bridge = await createLoopBridge({ emit: (event) => events.push(event) });
  t.after(() => bridge.close());
  const [next, stop] = piLoopTools({ COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true', COLONIZER_BRIDGE_URL: bridge.url, COLONIZER_BRIDGE_TOKEN: bridge.token });
  assert.equal(next.parameters.type, 'object');
  const text = async (tool, params) => (await tool.execute('call-1', params)).content[0].text;
  assert.equal(await text(next, { delay_minutes: 3, reason: 'watch the build' }), 'Next run scheduled in 15 minutes.');
  assert.match(await text(stop, {}), /^Could not stop the loop: reason is required/);
  assert.equal(await text(stop, { reason: 'all green' }), 'The loop is stopped; this is its last run.');
  assert.deepEqual(events, [
    { type: 'loop_next', delay_minutes: 15, reason: 'watch the build' },
    { type: 'loop_stop', reason: 'all green' },
  ]);
  // A wrong token is refused by the bridge, and the tool reports it as a failed call.
  const [forged] = piLoopTools({ COLONIZER_LOOP: 'true', COLONIZER_BRIDGE_URL: bridge.url, COLONIZER_BRIDGE_TOKEN: 'nope' });
  await assert.rejects(forged.execute('call-2', { reason: 'x' }), /the colonizer bridge answered 401/);
  assert.equal(events.length, 2);
});

test('piArgs loads the loop extension only for a loop colony', () => {
  assert.ok(!piArgs({ provider: 'fake', modelId: 'm' }).includes(LOOP_EXTENSION));
  const loopArgs = piArgs({ provider: 'fake', modelId: 'm', loop: true });
  assert.deepEqual(loopArgs.slice(4, 6), ['--extension', LOOP_EXTENSION]);
});

// --- the real Pi --------------------------------------------------------------------------------

/** A stand-in Anthropic Messages endpoint: a fresh user message gets a loop_next tool_use, and a
 * tool result gets plain text. Every request is recorded. */
async function fakeAnthropic() {
  const requests = [];
  const message = { id: 'msg', type: 'message', role: 'assistant', model: 'm', content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 1 } };
  const stream = (res, block, delta, stop) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    for (const [type, data] of [['message_start', { message }], ['content_block_start', { index: 0, content_block: block }], ['content_block_delta', { index: 0, delta }], ['content_block_stop', { index: 0 }], ['message_delta', { delta: { stop_reason: stop }, usage: { output_tokens: 1 } }], ['message_stop', {}]]) {
      res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
    }
    res.end();
  };
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      const parsed = JSON.parse(body);
      requests.push(parsed);
      const last = parsed.messages.at(-1);
      const answered = Array.isArray(last.content) && last.content.some((part) => part.type === 'tool_result');
      if (answered) stream(res, { type: 'text', text: '' }, { type: 'text_delta', text: 'done' }, 'end_turn');
      else stream(res, { type: 'tool_use', id: 'tu-1', name: 'loop_next', input: {} }, { type: 'input_json_delta', partial_json: '{"delay_minutes":90,"reason":"the nightly build lands then"}' }, 'tool_use');
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { requests, url: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((resolve) => (server.close(resolve), server.closeAllConnections?.())) };
}

test('the real Pi offers the loop tools to a self-paced loop colony, and its loop_next call leaves as a loop_next event', async (t) => {
  const api = await fakeAnthropic();
  const agentDir = mkdtempSync(join(tmpdir(), 'pi-agent-'));
  t.after(async () => {
    await api.close();
    rmSync(agentDir, { recursive: true, force: true });
  });
  const { routes } = parseRoutes(JSON.stringify([{ prefix: 'fake/', base_url: api.url, auth: 'none' }]));
  writeFileSync(join(agentDir, 'models.json'), JSON.stringify(buildModelsConfig(routes, 'fake/m')));
  const commands = commandQueue();
  const events = [];
  const until = async (check) => {
    const deadline = Date.now() + 30_000;
    while (!check()) {
      assert.ok(Date.now() < deadline, `timed out; events: ${JSON.stringify(events)}`);
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
  };
  const env = piEnv({ PATH: process.env.PATH, HOME: agentDir, COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true' }, agentDir);
  const done = runAgent({ commands, emit: (event) => events.push(event), selection: { route: routes[0], provider: 'fake', modelId: 'm' }, env, cwd: agentDir, graceMs: 2000 });

  commands.push({ type: 'user_message', text: 'loop run' });
  await until(() => events.some((event) => event.type === 'turn_end'));
  assert.deepEqual(api.requests[0].tools.map((tool) => tool.name).filter((name) => name.startsWith('loop_')), ['loop_next', 'loop_stop']);
  assert.deepEqual(events.find((event) => event.type === 'loop_next'), { type: 'loop_next', delay_minutes: 90, reason: 'the nightly build lands then' });
  assert.match(JSON.stringify(api.requests.at(-1).messages.at(-1)), /Next run scheduled in 90 minutes\./);

  commands.push({ type: 'shutdown' });
  await done;
});
