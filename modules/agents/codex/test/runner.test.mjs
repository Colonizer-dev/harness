// Contract tests for the codex runner: they boot the real runner.mjs as a child process and drive
// it over the colonizer-runner/1 protocol against test/fake-codex.mjs standing in for the codex
// CLI (COLONIZER_CODEX_BIN is the seam). The happy path's events are also checked against the
// required fields of docs/agent-events.schema.json.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { parseVersion, resolveModel, turnArgs } from '../runner.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const moduleDir = join(here, '..');
const fakeCodex = join(here, 'fake-codex.mjs');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** The [flag, value] pair an argv carries, or null when the flag is absent. */
const pair = (args, flag) => (args.includes(flag) ? args.slice(args.indexOf(flag), args.indexOf(flag) + 2) : null);

/** The runner as a child process, with its protocol output collected into `events`. */
function startRunner(env = {}) {
  // A scratch dir plus a `codex` on it that is the fake, so COLONIZER_CODEX_BIN needs no PATH games.
  const binDir = mkdtempSync(join(tmpdir(), 'codex-test-bin-'));
  const bin = join(binDir, 'codex');
  writeFileSync(bin, `#!/bin/sh\nexec ${process.execPath} ${JSON.stringify(fakeCodex)} "$@"\n`, { mode: 0o755 });
  const scratch = mkdtempSync(join(tmpdir(), 'codex-test-'));
  const record = join(scratch, 'record.jsonl');
  const child = spawn(process.execPath, [join(moduleDir, 'runner.mjs')], {
    env: { PATH: '/usr/bin:/bin', TMPDIR: scratch, CODEX_API_KEY: 'codex-test-key', COLONIZER_CODEX_BIN: bin, CODEX_FAKE_RECORD: record, ...env },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const events = [];
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  lines.on('line', (line) => {
    if (line.trim()) {
      try {
        events.push(JSON.parse(line));
      } catch {
        events.push({ type: 'unparseable', line });
      }
    }
  });

  const send = (command) => child.stdin.write(`${JSON.stringify(command)}\n`);
  // `check` sees the whole event list, so a test can wait on a count, not just a first match.
  const waitUntil = async (check, what, timeoutMs = 20000) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const found = check(events);
      if (found) return found;
      if (child.exitCode !== null || Date.now() > deadline) {
        assert.fail(`timed out waiting for ${what}; runner exit ${child.exitCode}; events: ${JSON.stringify(events)}`);
      }
      await sleep(25);
    }
  };
  let exitCode = null;
  child.on('exit', (code) => (exitCode = code));
  const waitExit = async (timeoutMs = 10000) => {
    const deadline = Date.now() + timeoutMs;
    while (exitCode === null && Date.now() < deadline) await sleep(10);
    return exitCode ?? 'timeout';
  };
  const records = () => (existsSync(record) ? readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  // Only the turn invocations carry --json; the records also hold the preflight's `--version` call.
  const turns = () => records().filter((r) => r.argv.includes('--json'));
  return { child, events, send, waitUntil, waitExit, records, turns };
}

const stop = (runner) => (runner.send({ type: 'shutdown' }), runner.waitExit());

// docs/agent-events.schema.json, reduced to the required fields per event type we emit.
const schema = JSON.parse(readFileSync(join(moduleDir, '..', '..', '..', 'docs', 'agent-events.schema.json'), 'utf8'));
const required = Object.fromEntries(Object.entries(schema.$defs ?? {}).map(([name, def]) => [name, def.required ?? []]));

function assertSchema(events) {
  for (const event of events) {
    for (const field of required[event.type] ?? []) {
      assert.ok(event[field] !== undefined, `${event.type} is missing the schema-required field "${field}"`);
    }
  }
}

const first = (type) => (events) => events.find((e) => e.type === type);
const count = (type, n) => (events) => {
  const matches = events.filter((e) => e.type === type);
  return matches.length >= n ? matches[n - 1] : undefined;
};

test('parseVersion takes the first semver, wherever it sits', () => {
  assert.equal(parseVersion('codex-cli 0.156.1'), '0.156.1');
  assert.equal(parseVersion('1.2.3'), '1.2.3');
  assert.equal(parseVersion('no digits here'), null);
  assert.equal(parseVersion(undefined), null);
});

test('resolveModel accepts openai and bare ids, keeps empties for the CLI default, and refuses other providers by name', () => {
  assert.deepEqual(resolveModel('openai/gpt-5.2'), { model: 'gpt-5.2' });
  assert.deepEqual(resolveModel('  gpt-5.3-codex '), { model: 'gpt-5.3-codex' });
  assert.deepEqual(resolveModel(''), {});
  assert.match(resolveModel('deepseek/deepseek-flash').error, /^CODEX_MODEL_PROVIDER:/);
});

test('turnArgs carries the --json framing, the nesting overrides, and the resume subcommand order', () => {
  const args = turnArgs({ model: 'gpt-5.2', threadId: 'thread-1' });
  for (const flag of ['--json', '--skip-git-repo-check', '--dangerously-bypass-approvals-and-sandbox']) {
    assert.ok(args.includes(flag), `missing ${flag}`);
  }
  assert.deepEqual(pair(args, '-c'), ['-c', 'check_for_update_on_startup=false'], 'the -c overrides ride exec level, before resume');
  assert.deepEqual(pair(args, '-m'), ['-m', 'gpt-5.2']);
  assert.ok(args.indexOf('-m') < args.indexOf('resume'), 'exec-level options come before the resume subcommand');
  assert.deepEqual(args.slice(args.indexOf('resume')), ['resume', 'thread-1', '-'], 'resume names the thread, then the stdin prompt');
  const fresh = turnArgs({});
  assert.ok(!fresh.includes('-m') && !fresh.includes('resume'), 'no model and nothing to resume means neither');
  assert.equal(fresh[fresh.length - 1], '-', 'the prompt is always stdin');
});

test('a turn streams mapped events and the child carries the nesting env', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'openai/gpt-5.2' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'initial', text: 'do the thing' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(runner.events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(first('user_message')(runner.events), { type: 'user_message', id: 'initial', text: 'do the thing' });
  const order = runner.events.map((e) => `${e.type}:${e.state ?? ''}`);
  assert.ok(order.indexOf('user_message:') < order.indexOf('status:working'), 'working follows the echo');
  assert.ok(order.indexOf('status:working') < order.indexOf('turn_end:'), 'the turn ends inside working');
  assert.ok(order.indexOf('turn_end:') < order.indexOf('status:idle', 1), 'idle follows the turn end');
  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'gpt-5.2', previous: null });
  assert.deepEqual(first('thinking')(runner.events), { type: 'thinking', message_id: 'msg-1', block_index: 1, text: 'weighing the options' });
  assert.deepEqual(first('assistant_text')(runner.events), { type: 'assistant_text', message_id: 'msg-1', block_index: 0, text: 'Hello, colony' });
  assert.equal(runner.events.filter((e) => e.type === 'assistant_text_delta').length, 0, 'codex --json has no deltas; the message arrives whole');
  assert.deepEqual(first('tool_call')(runner.events), {
    type: 'tool_call',
    message_id: 'msg-1',
    tool_call_id: 'item_1',
    name: 'command_execution',
    input: { command: 'bash -lc ls' },
  });
  assert.deepEqual(first('tool_result')(runner.events), { type: 'tool_result', tool_call_id: 'item_1', output: 'src\nREADME.md', is_error: false });
  assert.equal(runner.events.filter((e) => e.type === 'tool_result').length, 1, 'the in_progress item emits no tool_result');

  const turnEnd = first('turn_end')(runner.events);
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'Hello, colony');
  assert.equal(turnEnd.cost_usd, null, 'codex reports tokens, never cost');
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assert.deepEqual(turnEnd.model_usage, { 'gpt-5.2': { input_tokens: 10, output_tokens: 5, cache_read_tokens: 2, cache_write_tokens: 0 } });
  assertSchema(runner.events);

  const [invocation] = runner.turns();
  assert.equal(invocation.prompt, 'do the thing', 'the prompt travels on stdin');
  assert.ok(!invocation.argv.includes('login'), 'the runner never logs in');
  assert.ok(!invocation.argv.includes('--ephemeral'), 'the session rollout persists: resume needs it');
  assert.deepEqual(pair(invocation.argv, '-m'), ['-m', 'gpt-5.2']);
  assert.ok(!invocation.argv.includes('resume'), 'the first turn starts a fresh thread');
  assert.equal(invocation.argv[invocation.argv.length - 1], '-', 'the prompt is the stdin sentinel');
  assert.equal(invocation.env.BROWSER, '/bin/false');
  assert.equal(invocation.env.CODEX_API_KEY, 'set');
  assert.ok(invocation.env.CODEX_HOME && invocation.env.CODEX_HOME !== process.env.CODEX_HOME, 'a fresh CODEX_HOME is assigned');

  runner.send({ type: 'shutdown' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('an OPENAI_API_KEY colony reaches codex as CODEX_API_KEY, the only name codex exec reads', async (t) => {
  const runner = startRunner({ CODEX_API_KEY: '', OPENAI_API_KEY: 'openai-fallback-key' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'go' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');
  assert.equal(runner.turns()[0].env.CODEX_API_KEY, 'set', 'the fallback credential rides under the real name');

  await stop(runner);
});

test('the second turn resumes the first turn’s thread, with cumulative token totals', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'openai/gpt-5.2' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'turn one' });
  await runner.waitUntil(count('turn_end', 1), 'the first turn to finish');
  runner.send({ type: 'user_message', id: 'u-2', text: 'turn two' });
  const second = await runner.waitUntil(count('turn_end', 2), 'the second turn to finish');

  const turns = runner.turns();
  assert.equal(turns.length, 2);
  assert.ok(!turns[0].argv.includes('resume'), 'the first turn starts a fresh thread');
  assert.deepEqual(turns[1].argv.slice(turns[1].argv.indexOf('resume'), -1), ['resume', 'thread-fake-1'], 'the second turn resumes thread.started’s id');
  assert.deepEqual(second.model_usage, { 'gpt-5.2': { input_tokens: 20, output_tokens: 10, cache_read_tokens: 4, cache_write_tokens: 0 } }, 'model_usage is cumulative for the colony');

  await stop(runner);
});

test('set_model switches the next turn and announces it; other providers are refused by name', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'openai/gpt-5.2' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'set_model', model: 'deepseek/deepseek-flash' });
  const refusal = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the provider refusal');
  assert.match(refusal.message, /^CODEX_MODEL_PROVIDER:/);

  runner.send({ type: 'set_model', model: 'openai/gpt-5.3-codex' });
  await runner.waitUntil(first('model_changed'), 'model_changed');
  runner.send({ type: 'user_message', id: 'u-1', text: 'go' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'gpt-5.3-codex', previous: null });
  assert.equal(runner.events.filter((e) => e.type === 'model_changed').length, 1, 'a set_model already announced suppresses the first-turn announcement');
  assert.deepEqual(pair(runner.turns()[0].argv, '-m'), ['-m', 'gpt-5.3-codex']);

  await stop(runner);
});

test('interrupt fails the running turn, and the runner keeps serving turns', async (t) => {
  const runner = startRunner({ CODEX_FAKE_SLEEP_MS: 1500, CODEX_FAKE_SLEEP_FIRST: '1' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'slow turn' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'working'), 'the slow turn to start');
  runner.send({ type: 'interrupt' });
  const interrupted = await runner.waitUntil(first('turn_end'), 'the interrupted turn to end');
  assert.equal(interrupted.is_error, true);
  assert.match(interrupted.result, /interrupted/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the interrupt');

  runner.send({ type: 'user_message', id: 'u-2', text: 'quick turn' });
  const recovered = await runner.waitUntil(count('turn_end', 2), 'the next turn to finish');
  assert.equal(recovered.is_error, false);
  assert.equal(recovered.result, 'Hello, colony');

  await stop(runner);
});

test('shutdown mid-turn exits cleanly with code 0', async (t) => {
  const runner = startRunner({ CODEX_FAKE_SLEEP_MS: 60000 });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'endless turn' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'working'), 'the turn to start');
  runner.send({ type: 'shutdown' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('an answer is logged and ignored (no question path); stdin EOF exits like shutdown', async (t) => {
  const runner = startRunner();
  t.after(() => runner.child.kill('SIGKILL'));
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'the runner to come up');
  runner.send({ type: 'answer', question_id: 'q-1', answers: {}, response: 'yes' });
  const logged = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.message.includes('answer')), 'the answer to be logged');
  assert.equal(logged.level, 'info');
  runner.child.stdin.end();
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('without CODEX_API_KEY: a named error, and codex is never invoked', async (t) => {
  const runner = startRunner({ CODEX_API_KEY: '' });
  t.after(() => runner.child.kill('SIGKILL'));

  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the credential error');
  assert.match(problem.message, /^CODEX_CREDENTIAL_MISSING:/);
  assert.match(problem.message, /colony secret named CODEX_API_KEY/);
  const error = await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error'), 'the error status');
  assert.equal(error.detail, 'CODEX_CREDENTIAL_MISSING');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^CODEX_CREDENTIAL_MISSING:/);
  assert.equal(turnEnd.cost_usd, null);

  assert.equal(runner.records().length, 0, 'not even codex --version may run without a key');
  await stop(runner);
});

test('a missing binary is a named error carrying the pinned install command', async (t) => {
  const runner = startRunner({ COLONIZER_CODEX_BIN: '/nonexistent/codex' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the binary error');
  assert.match(problem.message, /^CODEX_BINARY_MISSING:/);
  assert.match(problem.message, /npm install -g @openai\/codex@0\.156\.1/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'CODEX_BINARY_MISSING'), 'the error status');
  await stop(runner);
});

test('a codex that is not the pinned version is a named drift error', async (t) => {
  const runner = startRunner({ CODEX_FAKE_VERSION: '0.99.0' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the drift error');
  assert.match(problem.message, /^CODEX_VERSION_DRIFT:/);
  assert.match(problem.message, /0\.99\.0/);
  assert.match(problem.message, /0\.156\.1/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'CODEX_VERSION_DRIFT'), 'the error status');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^CODEX_VERSION_DRIFT:/);
  assert.equal(runner.records().filter((r) => !r.argv.includes('--version')).length, 0, 'no prompt may run on a drifted binary');
  await stop(runner);
});

test('unknown event types, non-JSON lines and a turn.failed are survived or surfaced', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'codex-test-script-'));
  const scriptPath = join(dir, 'events.ndjson');
  writeFileSync(
    scriptPath,
    [
      JSON.stringify({ type: 'mystery', payload: 1 }),
      'not json at all',
      JSON.stringify({ type: 'item.completed', item: { id: 'item_1', type: 'agent_message', text: 'still fine' } }),
      JSON.stringify({ type: 'turn.failed', error: { message: 'unexpected status 401 Unauthorized' } }),
    ].join('\n'),
  );
  const runner = startRunner({ CODEX_FAKE_SCRIPT: scriptPath });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'odd stream' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('mystery')), 'the unknown-event warning');
  await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('not JSON')), 'the non-JSON line warning');
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the failed turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /401 Unauthorized/, 'turn.failed names the upstream error');

  await stop(runner);
});

test('a child that exits without turn.completed fails the turn with the exit code and stderr tail', async (t) => {
  const runner = startRunner({ CODEX_FAKE_NO_COMPLETE: '1', CODEX_FAKE_EXIT: '1', CODEX_FAKE_STDERR: 'boom: stream broke' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'doomed' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the failed turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /without a turn\.completed event/);
  assert.match(turnEnd.result, /exit code 1/);
  assert.match(turnEnd.result, /boom: stream broke/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the failed turn');
  await stop(runner);
});
