// Contract tests for the grok-build runner: they boot the real runner.mjs as a child process and
// drive it over the colonizer-runner/1 protocol against test/fake-grok.mjs standing in for the
// grok CLI (COLONIZER_GROK_BIN is the seam). The happy path's events are also checked against the
// required fields of docs/agent-events.schema.json.

import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { configToml, createBridge, parseVersion, resolveModel, startTurn, turnArgs, untrustableWorkspace } from '../runner.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const moduleDir = join(here, '..');
const fakeGrok = join(here, 'fake-grok.mjs');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** The [flag, value] pair an argv carries, or null when the flag is absent. */
const pair = (args, flag) => (args.includes(flag) ? args.slice(args.indexOf(flag), args.indexOf(flag) + 2) : null);

/** The runner as a child process, with its protocol output collected into `events`. */
function startRunner(env = {}, spawnOpts = {}) {
  // A scratch dir plus a `grok` on it that is the fake, so COLONIZER_GROK_BIN needs no PATH games.
  const binDir = mkdtempSync(join(tmpdir(), 'grok-test-bin-'));
  const bin = join(binDir, 'grok');
  writeFileSync(bin, `#!/bin/sh\nexec ${process.execPath} ${JSON.stringify(fakeGrok)} "$@"\n`, { mode: 0o755 });
  const scratch = mkdtempSync(join(tmpdir(), 'grok-test-'));
  const record = join(scratch, 'record.jsonl');
  const child = spawn(process.execPath, [join(moduleDir, 'runner.mjs')], {
    env: { PATH: '/usr/bin:/bin', TMPDIR: scratch, XAI_API_KEY: 'xai-test-key', COLONIZER_GROK_BIN: bin, GROK_FAKE_RECORD: record, ...env },
    stdio: ['pipe', 'pipe', 'pipe'],
    ...spawnOpts,
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
  child.stderr.setEncoding('utf8');

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
  // Attached at spawn, not on demand: the runner exits right after its final event, so a late
  // `once(child, "exit")` would race an exit that already happened.
  let exitCode = null;
  child.on('exit', (code) => (exitCode = code));
  const waitExit = async (timeoutMs = 10000) => {
    const deadline = Date.now() + timeoutMs;
    while (exitCode === null && Date.now() < deadline) await sleep(10);
    return exitCode ?? 'timeout';
  };
  const records = () => (existsSync(record) ? readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  // Only the prompt invocations are turns; the records also hold the preflight's `--version` call.
  const turns = () => records().filter((r) => r.argv.includes('--prompt-file'));
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

/** A workspace carrying every project-scope grok surface the folder-trust gate covers: a `.grok/`
 * with an MCP server, a plugin path, a hook and a skill, plus a `.mcp.json` beside them. */
function projectScopeWorkspace(dir) {
  mkdirSync(join(dir, '.grok', 'hooks'), { recursive: true });
  mkdirSync(join(dir, '.grok', 'skills', 'evil-skill'), { recursive: true });
  mkdirSync(join(dir, '.grok', 'plugins', 'evil-plugin', '.grok-plugin'), { recursive: true });
  writeFileSync(join(dir, '.grok', 'config.toml'), '[mcp_servers.evil]\ncommand = "/bin/echo"\nargs = ["evil-grok-project-mcp"]\n\n[plugins]\npaths = [".grok/plugins/evil-plugin"]\n');
  writeFileSync(join(dir, '.grok', 'hooks', 'hooks.json'), `${JSON.stringify({ hooks: { UserPromptSubmit: [{ hooks: [{ type: 'command', command: '/bin/echo evil-project-hook' }] }] } })}\n`);
  writeFileSync(join(dir, '.grok', 'skills', 'evil-skill', 'SKILL.md'), '---\nname: evil-skill\ndescription: project-scope skill\n---\nEvil.\n');
  writeFileSync(join(dir, '.grok', 'plugins', 'evil-plugin', '.grok-plugin', 'plugin.json'), `${JSON.stringify({ name: 'evil-plugin', version: '0.0.1' })}\n`);
  writeFileSync(join(dir, '.mcp.json'), `${JSON.stringify({ mcpServers: { 'evil-json': { command: '/bin/echo', args: ['evil-json-mcp'] } } })}\n`);
}

test('parseVersion takes the first semver, wherever it sits', () => {
  assert.equal(parseVersion('grok 1.0.34 (built today)'), '1.0.34');
  assert.equal(parseVersion('1.2.3'), '1.2.3');
  assert.equal(parseVersion('no digits here'), null);
  assert.equal(parseVersion(undefined), null);
});

test('resolveModel accepts xai-grok and bare ids, and refuses other providers by name', () => {
  assert.deepEqual(resolveModel('xai-grok/grok-4.5'), { model: 'grok-4.5' });
  assert.deepEqual(resolveModel('  grok-4.6 '), { model: 'grok-4.6' });
  assert.deepEqual(resolveModel(''), {});
  assert.match(resolveModel('deepseek/deepseek-flash').error, /^GROK_MODEL_PROVIDER:/);
});

test('untrustableWorkspace refuses only a workspace that is the home directory or the filesystem root', () => {
  const home = mkdtempSync(join(tmpdir(), 'grok-test-untrust-'));
  assert.equal(untrustableWorkspace(home, home), home, 'a workspace that is $HOME is auto-trusted upstream');
  assert.equal(untrustableWorkspace('/', home), '/', 'the filesystem root can never be gated');
  assert.equal(untrustableWorkspace(home, mkdtempSync(join(tmpdir(), 'grok-test-other-'))), null, 'an ordinary workspace gates fine');
  // A checkout merely inside $HOME keys on the checkout itself upstream (workspace_key falls back
  // to the cwd when the git root is over-broad), so it still gates.
  const nested = join(home, 'nested');
  mkdirSync(nested, { recursive: true });
  assert.equal(untrustableWorkspace(nested, home), null, 'a checkout inside $HOME keys on itself and gates');
});

test('turnArgs carries every nesting flag, the model, and the resume id', () => {
  const args = turnArgs({ promptFile: '/tmp/p.txt', model: 'grok-4.5', sessionId: 'sess-1' });
  for (const flag of ['--output-format', 'streaming-json', '--always-approve', '--sandbox', 'off', '--disable-web-search', '--no-auto-update']) {
    assert.ok(args.includes(flag), `missing ${flag}`);
  }
  assert.deepEqual(pair(args, '-m'), ['-m', 'grok-4.5']);
  assert.deepEqual(pair(args, '-r'), ['-r', 'sess-1']);
  const bare = turnArgs({ promptFile: '/tmp/p.txt' });
  assert.ok(!bare.includes('-m') && !bare.includes('-r'), 'no model and nothing to resume means neither flag');
  assert.ok(!bare.includes('--disallowed-tools'), 'an empty disabled_tools list passes no denylist');
});

test('turnArgs passes a non-empty disabled_tools list as --disallowed-tools', () => {
  const args = turnArgs({ promptFile: '/tmp/p.txt', model: 'grok-4.5', sessionId: 'sess-1', disabledTools: ['run_terminal_cmd', 'write_file'] });
  assert.deepEqual(pair(args, '--disallowed-tools'), ['--disallowed-tools', 'run_terminal_cmd,write_file'], 'the denylist rides comma-joined');
  assert.ok(args.indexOf('--disallowed-tools') < args.indexOf('-m'), 'the denylist rides with the other headless flags');
});

test('a turn streams mapped events and the child carries the nesting flags', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'xai-grok/grok-4.5' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'initial', text: 'do the thing' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(runner.events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(first('user_message')(runner.events), { type: 'user_message', id: 'initial', text: 'do the thing' });
  const order = runner.events.map((e) => `${e.type}:${e.state ?? ''}`);
  assert.ok(order.indexOf('status:idle') < order.indexOf('user_message:'), 'the echo follows the initial idle');
  assert.ok(order.indexOf('user_message:') < order.indexOf('status:working'), 'working follows the echo');
  assert.ok(order.indexOf('status:working') < order.indexOf('turn_end:'), 'the turn ends inside working');
  assert.ok(order.indexOf('turn_end:') < order.indexOf('status:idle', 1), 'idle follows the turn end');
  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'grok-4.5', previous: null });
  assert.deepEqual(
    runner.events.filter((e) => e.type === 'assistant_text_delta').map((e) => e.delta),
    ['Hello', ', colony'],
  );
  assert.deepEqual(first('thinking')(runner.events), { type: 'thinking', message_id: 'msg-1', block_index: 1, text: 'weighing the options' });
  assert.deepEqual(first('assistant_text')(runner.events), { type: 'assistant_text', message_id: 'msg-1', block_index: 0, text: 'Hello, colony' });
  assert.deepEqual(first('tool_call')(runner.events), {
    type: 'tool_call',
    message_id: 'msg-1',
    tool_call_id: 'call_1',
    name: 'read_file',
    input: { path: 'src/main.rs' },
  });
  assert.deepEqual(first('tool_result')(runner.events), { type: 'tool_result', tool_call_id: 'call_1', output: '{"lines":42}', is_error: false });
  assert.equal(runner.events.filter((e) => e.type === 'tool_result').length, 1, 'an in_progress update emits no tool_result');

  const turnEnd = first('turn_end')(runner.events);
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'Hello, colony');
  assert.equal(turnEnd.cost_usd, 0.01);
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assert.deepEqual(turnEnd.model_usage, { 'grok-4.5': { input_tokens: 10, output_tokens: 5, cache_read_tokens: 2, cache_write_tokens: 0 } });
  assertSchema(runner.events);

  const [invocation] = runner.turns();
  assert.equal(invocation.prompt, 'do the thing', 'the prompt travels via --prompt-file');
  assert.ok(!invocation.argv.includes('-p'), 'no -p: the prompt rides in a file');
  assert.ok(!invocation.argv.includes('login'), 'the runner never logs in');
  assert.deepEqual(pair(invocation.argv, '-m'), ['-m', 'grok-4.5']);
  assert.ok(!invocation.argv.includes('-r'), 'the first turn starts a fresh session');
  assert.equal(invocation.env.BROWSER, '/bin/false');
  assert.equal(invocation.env.GROK_MEMORY, '0');
  assert.equal(invocation.env.GROK_TELEMETRY_ENABLED, '0');
  assert.equal(invocation.env.GROK_DISABLE_AUTOUPDATER, '1');
  assert.equal(invocation.env.XAI_API_KEY, 'set');
  assert.ok(invocation.env.GROK_HOME && invocation.env.GROK_HOME !== process.env.GROK_HOME, 'a fresh GROK_HOME is assigned');

  runner.send({ type: 'shutdown' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('project-scope config stays off: the gate is forced past an inherited GROK_FOLDER_TRUST=0, headless, with no --trust and no trust store', async (t) => {
  const workspace = mkdtempSync(join(tmpdir(), 'grok-test-ws-'));
  projectScopeWorkspace(workspace);
  const runner = startRunner({ GROK_FOLDER_TRUST: '0' }, { cwd: workspace });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'do the thing' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  const [invocation] = runner.turns();
  assert.equal(invocation.env.GROK_FOLDER_TRUST, '1', 'the gate is forced on; the host value must never pass through');
  assert.ok(!invocation.argv.includes('--trust'), 'the runner never grants folder trust itself');
  assert.equal(invocation.trustedFolders, false, 'the fresh GROK_HOME starts with an empty trust store');
  assert.equal(invocation.tty.stdin, false, 'stdin is a pipe: headless resolves untrusted, never a trust prompt');
  assert.equal(invocation.tty.stderr, false, 'stderr is a pipe too');
  assert.ok(invocation.env.GROK_HOME && !invocation.env.GROK_HOME.startsWith(workspace), 'the fresh GROK_HOME is not inside the workspace');

  await stop(runner);
});

test('the second turn resumes the first turn’s grok session, with cumulative cost and usage', async (t) => {
  const runner = startRunner();
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'turn one' });
  await runner.waitUntil(count('turn_end', 1), 'the first turn to finish');
  runner.send({ type: 'user_message', id: 'u-2', text: 'turn two' });
  const second = await runner.waitUntil(count('turn_end', 2), 'the second turn to finish');

  const turns = runner.turns();
  assert.equal(turns.length, 2);
  assert.ok(!turns[0].argv.includes('-r'), 'the first turn starts a fresh session');
  assert.deepEqual(pair(turns[1].argv, '-r'), ['-r', 'sess-fake-1'], 'the second turn resumes the end event’s sessionId');
  assert.equal(second.cost_usd, 0.02, 'cost_usd is cumulative for the colony');
  assert.deepEqual(second.model_usage, { 'grok-4.5': { input_tokens: 20, output_tokens: 10, cache_read_tokens: 4, cache_write_tokens: 0 } });

  await stop(runner);
});

test('a disabled_tools colony denies the named tools on every turn, resume included', async (t) => {
  const runner = startRunner({ COLONIZER_DISABLED_TOOLS: 'run_terminal_cmd, write_file' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'turn one' });
  await runner.waitUntil(count('turn_end', 1), 'the first turn to finish');
  runner.send({ type: 'user_message', id: 'u-2', text: 'turn two' });
  await runner.waitUntil(count('turn_end', 2), 'the second turn to finish');

  const turns = runner.turns();
  for (const [i, invocation] of turns.entries()) {
    assert.deepEqual(pair(invocation.argv, '--disallowed-tools'), ['--disallowed-tools', 'run_terminal_cmd,write_file'], `turn ${i + 1} carries the denylist`);
  }

  await stop(runner);
});

test('set_model switches the next turn and announces it; other providers are refused by name', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'xai-grok/grok-4.5' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'set_model', model: 'deepseek/deepseek-flash' });
  const refusal = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the provider refusal');
  assert.match(refusal.message, /^GROK_MODEL_PROVIDER:/);

  runner.send({ type: 'set_model', model: 'xai-grok/grok-5' });
  await runner.waitUntil(first('model_changed'), 'model_changed');
  runner.send({ type: 'user_message', id: 'u-1', text: 'go' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'grok-5', previous: null });
  assert.equal(runner.events.filter((e) => e.type === 'model_changed').length, 1, 'a set_model already announced suppresses the first-turn announcement');
  assert.deepEqual(pair(runner.turns()[0].argv, '-m'), ['-m', 'grok-5']);

  await stop(runner);
});

test('interrupt fails the running turn, and the runner keeps serving turns', async (t) => {
  const runner = startRunner({ GROK_FAKE_SLEEP_MS: 1500, GROK_FAKE_SLEEP_FIRST: '1' });
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

test('an interrupt that lands before the spawn ends the turn without starting grok', async () => {
  // startTurn returns while its body is still awaiting the prompt-file write, so an interrupt()
  // fired straight after always lands in the window between `status working` and the spawn —
  // deterministically, with no timers involved.
  const events = [];
  let spawns = 0;
  const spawnFn = () => {
    spawns += 1;
    throw new Error('the spawn point must not be reached');
  };
  const home = mkdtempSync(join(tmpdir(), 'grok-pre-spawn-'));
  const turn = startTurn({
    prompt: 'interrupted before grok starts',
    model: null,
    sessionId: null,
    messageId: 'msg-1',
    env: {},
    home,
    emit: (event) => events.push(event),
    spawnFn,
    totals: { cost: undefined, models: {} },
  });
  turn.interrupt();
  const result = await turn.done;

  assert.equal(spawns, 0, 'grok is never invoked');
  assert.deepEqual(result, { sessionId: null, failure: 'interrupted by the user' });
  const turnEnd = events.find((e) => e.type === 'turn_end');
  assert.equal(turnEnd.is_error, true);
  assert.equal(turnEnd.result, 'interrupted by the user');
  assert.equal(turnEnd.cost_usd, null);
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assert.ok(!existsSync(join(home, 'msg-1.prompt.txt')), 'the prompt file is cleaned up');
});

test('shutdown mid-turn exits cleanly with code 0', async (t) => {
  const runner = startRunner({ GROK_FAKE_SLEEP_MS: 60000 });
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

test('without XAI_API_KEY: a named error, and grok is never invoked', async (t) => {
  const runner = startRunner({ XAI_API_KEY: '' });
  t.after(() => runner.child.kill('SIGKILL'));

  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the credential error');
  assert.match(problem.message, /^GROK_CREDENTIAL_MISSING:/);
  assert.match(problem.message, /colony secret named XAI_API_KEY/);
  const error = await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error'), 'the error status');
  assert.equal(error.detail, 'GROK_CREDENTIAL_MISSING');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^GROK_CREDENTIAL_MISSING:/);
  assert.equal(turnEnd.cost_usd, null);

  assert.equal(runner.records().length, 0, 'not even grok --version may run without a key');
  await stop(runner);
});

test('a missing binary is a named error carrying the pinned install command', async (t) => {
  const runner = startRunner({ COLONIZER_GROK_BIN: '/nonexistent/grok' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the binary error');
  assert.match(problem.message, /^GROK_BINARY_MISSING:/);
  assert.match(problem.message, /curl -fsSL https:\/\/x\.ai\/cli\/install\.sh \| bash -s 1\.0\.34/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'GROK_BINARY_MISSING'), 'the error status');
  await stop(runner);
});

test('a grok that is not the pinned version is a named drift error', async (t) => {
  const runner = startRunner({ GROK_FAKE_VERSION: '0.9.9' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the drift error');
  assert.match(problem.message, /^GROK_VERSION_DRIFT:/);
  assert.match(problem.message, /0\.9\.9/);
  assert.match(problem.message, /1\.0\.34/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'GROK_VERSION_DRIFT'), 'the error status');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^GROK_VERSION_DRIFT:/);
  assert.equal(runner.records().filter((r) => !r.argv.includes('--version')).length, 0, 'no prompt may run on a drifted binary');
  await stop(runner);
});

test('a workspace folder trust cannot gate (the home directory) is a named error before any spawn', async (t) => {
  const workspace = mkdtempSync(join(tmpdir(), 'grok-test-home-'));
  projectScopeWorkspace(workspace);
  const runner = startRunner({ HOME: workspace }, { cwd: workspace });
  t.after(() => runner.child.kill('SIGKILL'));

  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the workspace error');
  assert.match(problem.message, /^GROK_WORKSPACE_UNTRUSTABLE:/);
  assert.match(problem.message, /dedicated worktree/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'GROK_WORKSPACE_UNTRUSTABLE'), 'the error status');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^GROK_WORKSPACE_UNTRUSTABLE:/);
  assert.equal(runner.records().length, 0, 'not even grok --version may run in an untrustable workspace');
  await stop(runner);
});

test('unknown streaming event types are warned about, not fatal', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'grok-test-script-'));
  const scriptPath = join(dir, 'events.ndjson');
  writeFileSync(
    scriptPath,
    [
      JSON.stringify({ type: 'mystery', payload: 1 }),
      JSON.stringify({ type: 'text', data: 'still fine' }),
      JSON.stringify({ type: 'end', sessionId: 'sess-x', stopReason: 'end_turn' }),
    ].join('\n'),
  );
  const runner = startRunner({ GROK_FAKE_SCRIPT: scriptPath });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'odd stream' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('mystery')), 'the unknown-event warning');
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the turn to finish anyway');
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'still fine');
  await stop(runner);
});

test('a child that exits without an end event fails the turn with the exit code and stderr tail', async (t) => {
  const runner = startRunner({ GROK_FAKE_NO_END: '1', GROK_FAKE_EXIT: '1', GROK_FAKE_STDERR: 'boom: auth rejected' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'doomed' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the failed turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /without an end event/);
  assert.match(turnEnd.result, /exit code 1/);
  assert.match(turnEnd.result, /boom: auth rejected/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the failed turn');
  await stop(runner);
});

test('the colonizer MCP tools work end to end: findings, memory and wait', async (t) => {
  const memory = mkdtempSync(join(tmpdir(), 'grok-test-mem-'));
  mkdirSync(join(memory, 'repo', 'notes'), { recursive: true });
  writeFileSync(join(memory, 'repo', 'notes', 'waiting.md'), '# Wait rooms\nThe waitrooms convention: call wait instead of polling.\n');
  const calls = [
    { name: 'finding_file', arguments: { title: 'Stale doc', body: 'The README lies about X.', evidence: 'read README.md twice' } },
    { name: 'memory_propose', arguments: { scope: 'repo', title: 'Run npm ci', content: 'The lockfile drifts without it.', tags: ['node'] } },
    { name: 'memory_search', arguments: { query: 'waitrooms' } },
    { name: 'wait', arguments: { reason: 'settling', seconds: 0 } },
  ];
  const runner = startRunner({
    COLONIZER_MODEL: 'xai-grok/grok-4.5',
    COLONIZER_FINDINGS: 'true',
    COLONIZER_MEMORY_DIR: memory,
    GROK_FAKE_MCP_CALLS: JSON.stringify(calls),
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'use the colonizer tools' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  // finding_file and memory_propose crossed the bridge and left the colony as protocol events.
  assert.deepEqual(first('finding')(runner.events), { type: 'finding', title: 'Stale doc', body: 'The README lies about X.', evidence: 'read README.md twice' });
  assert.deepEqual(first('memory_proposal')(runner.events), { type: 'memory_proposal', origin: 'orchestrator', scope: 'repo', title: 'Run npm ci', content: 'The lockfile drifts without it.', tags: ['node'] });
  assertSchema(runner.events);

  const { mcp } = runner.turns()[0];
  assert.deepEqual(mcp.tools, ['finding_file', 'memory_search', 'memory_propose', 'wait']);
  assert.deepEqual(mcp.calls[0], { name: 'finding_file', isError: false, text: '{"filed":true}', error: null });
  assert.deepEqual(mcp.calls[1], { name: 'memory_propose', isError: false, text: '{"ok":true}', error: null });
  assert.equal(mcp.calls[2].isError, false);
  assert.match(mcp.calls[2].text, /\[repo\] Wait rooms \(/);
  assert.match(mcp.calls[2].text, /waitrooms convention/);
  assert.match(mcp.calls[3].text, /^Waited .* \(settling\)\.$/);

  const mcpCalls = runner.events.filter((e) => e.type === 'tool_call' && String(e.name).startsWith('colonizer__'));
  assert.equal(mcpCalls.length, calls.length, 'the grok stream carries the calls under their colonizer__ names');
  assert.deepEqual(
    mcpCalls.map((e) => e.name),
    calls.map((c) => `colonizer__${c.name}`),
  );

  await stop(runner);
});

test('finding_file and the memory tools are only offered when the mothership switched them on', async (t) => {
  const runner = startRunner({ GROK_FAKE_MCP_CALLS: '[]' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'hello' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');
  assert.deepEqual(runner.turns()[0].mcp.tools, ['wait'], 'no findings switch and no memory dir leave only wait');

  await stop(runner);
});

test('configToml forwards the loop switches into the colonizer server env; undefined drops out', () => {
  const on = configToml({ url: 'http://127.0.0.1:9', token: 't', env: { COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true' } });
  assert.match(on, /COLONIZER_LOOP = "true"/);
  assert.match(on, /COLONIZER_LOOP_SELF_PACED = "true"/);
  // The end-to-end loop test below inherits the runner's env anyway, so this unit check is what
  // pins the forwarding: without the switches in the table, mcp.mjs never offers the loop tools.
  const off = configToml({ url: 'http://127.0.0.1:9', token: 't', env: {} });
  assert.ok(!off.includes('COLONIZER_LOOP'), 'undefined drops out of the inline table');
});

test('the bridge turns /loop_next and /loop_stop into protocol events and refuses malformed ones', async () => {
  const events = [];
  const bridge = await createBridge({ emit: (event) => events.push(event) });
  const post = (path, body) =>
    fetch(`${bridge.url}${path}`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${bridge.token}` }, body: JSON.stringify(body) }).then((r) => r.json());
  try {
    assert.deepEqual(await post('/loop_next', { delay_minutes: 90, reason: 'review comments are due' }), { ok: true });
    assert.deepEqual(await post('/loop_next', { delay_minutes: 3.5, reason: 'soon' }), { ok: true }, 'a fractional delay is rounded to the protocol\'s integer');
    assert.deepEqual(await post('/loop_next', {}), { error: 'loop_next needs delay_minutes: a number of minutes from now' });
    assert.deepEqual(await post('/loop_next', { delay_minutes: 30 }), { error: 'loop_next needs a reason: what the next run should find or do' });
    assert.deepEqual(await post('/loop_stop', { reason: 'the goal is met' }), { ok: true });
    assert.deepEqual(await post('/loop_stop', { reason: '  ' }), { error: 'loop_stop needs a reason: why the loop should stop' });
    assert.deepEqual(events, [
      { type: 'loop_next', delay_minutes: 90, reason: 'review comments are due' },
      { type: 'loop_next', delay_minutes: 4, reason: 'soon' },
      { type: 'loop_stop', reason: 'the goal is met' },
    ]);
  } finally {
    await bridge.close();
  }
});

test('a loop colony reports its pacing: loop_next with a clamped delay, and loop_stop', async (t) => {
  const calls = [
    { name: 'loop_next', arguments: { delay_minutes: 5, reason: 'review comments are due' } },
    { name: 'loop_stop', arguments: { reason: 'the goal is met' } },
  ];
  const runner = startRunner({ COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true', GROK_FAKE_MCP_CALLS: JSON.stringify(calls) });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'wrap up this run' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  // Both calls crossed the bridge and left the colony as protocol events, the delay clamped to the
  // mothership's 15-minute floor.
  assert.deepEqual(first('loop_next')(runner.events), { type: 'loop_next', delay_minutes: 15, reason: 'review comments are due' });
  assert.deepEqual(first('loop_stop')(runner.events), { type: 'loop_stop', reason: 'the goal is met' });
  assertSchema(runner.events);

  const { mcp } = runner.turns()[0];
  assert.deepEqual(mcp.tools, ['loop_next', 'loop_stop', 'wait']);
  assert.deepEqual(mcp.calls[0], { name: 'loop_next', isError: false, text: 'Next run scheduled in 15 minutes.', error: null });
  assert.deepEqual(mcp.calls[1], { name: 'loop_stop', isError: false, text: 'The loop is stopped; this is its last run.', error: null });

  const mcpCalls = runner.events.filter((e) => e.type === 'tool_call' && String(e.name).startsWith('colonizer__'));
  assert.deepEqual(mcpCalls.map((e) => e.name), ['colonizer__loop_next', 'colonizer__loop_stop'], 'the grok stream carries the calls under their colonizer__ names');

  await stop(runner);
});

test('a loop colony on a fixed schedule gets loop_stop but no loop_next', async (t) => {
  const runner = startRunner({ COLONIZER_LOOP: 'true', GROK_FAKE_MCP_CALLS: '[]' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'hello' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');
  assert.deepEqual(runner.turns()[0].mcp.tools, ['loop_stop', 'wait'], 'only a self-paced loop schedules its next run');

  await stop(runner);
});

// The live contract test behind COLONIZER_GROK_LIVE_BIN drives the real runner against the real
// pinned grok binary (a wrapper intercepts the turn and runs `grok inspect --json` in the
// workspace instead, which needs no xAI key), so it asserts what the forced gate actually makes
// grok resolve — not just what env the runner hands over. Run it with:
//   COLONIZER_GROK_LIVE_BIN=/path/to/grok npm test
const liveBin = String(process.env.COLONIZER_GROK_LIVE_BIN ?? '').trim();

test('live grok: the forced gate leaves the real binary with an untrusted workspace and none of the project config', { skip: liveBin ? false : 'set COLONIZER_GROK_LIVE_BIN to the pinned grok binary (install.sh 1.0.34) to run the live contract test' }, async (t) => {
  const workspace = mkdtempSync(join(tmpdir(), 'grok-live-ws-'));
  projectScopeWorkspace(workspace);
  // Folder trust keys the workspace on the git root; the parent env's GIT_DIR/GIT_WORK_TREE/
  // GIT_INDEX_FILE (a colonizer worktree sets them) would redirect the init into read-only rock.
  const gitEnv = { ...process.env };
  delete gitEnv.GIT_DIR;
  delete gitEnv.GIT_WORK_TREE;
  delete gitEnv.GIT_INDEX_FILE;
  execFileSync('git', ['init', '-q'], { cwd: workspace, env: gitEnv });
  const binDir = mkdtempSync(join(tmpdir(), 'grok-live-bin-'));
  const inspectJson = join(binDir, 'inspect.json');
  const facts = join(binDir, 'facts.txt');
  const wrapper = join(binDir, 'grok');
  // Preflight's `--version` passes through to the real binary; a turn becomes `inspect --json`
  // in the cwd the runner gave the child, followed by a bare end event so the turn completes.
  writeFileSync(
    wrapper,
    [
      '#!/bin/sh',
      'if [ "$1" = "--prompt-file" ]; then',
      '  {',
      '    printf \'argv=%s\\n\' "$*";',
      '    printf \'GROK_FOLDER_TRUST=%s\\n\' "${GROK_FOLDER_TRUST-unset}";',
      '    printf \'GROK_HOME=%s\\n\' "${GROK_HOME-unset}";',
      '    if [ -e "$GROK_HOME/trusted_folders.toml" ]; then printf \'trust_store=present\\n\'; else printf \'trust_store=absent\\n\'; fi',
      '    if [ -t 0 ]; then printf \'stdin=tty\\n\'; else printf \'stdin=pipe\\n\'; fi',
      '    if [ -t 2 ]; then printf \'stderr=tty\\n\'; else printf \'stderr=pipe\\n\'; fi',
      '  } > "$GROK_LIVE_FACTS"',
      `  "${liveBin}" inspect --json > "$GROK_LIVE_INSPECT" 2>/dev/null`,
      '  printf \'%s\\n\' \'{"type":"end","sessionId":"live-inspect","stopReason":"end_turn","total_cost_usd":0}\'',
      '  exit 0',
      'fi',
      `exec "${liveBin}" "$@"`,
      '',
    ].join('\n'),
    { mode: 0o755 },
  );
  const runner = startRunner(
    {
      COLONIZER_GROK_BIN: wrapper,
      GROK_LIVE_INSPECT: inspectJson,
      GROK_LIVE_FACTS: facts,
      HOME: mkdtempSync(join(tmpdir(), 'grok-live-home-')), // a home that is not the workspace
    },
    { cwd: workspace },
  );
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'inspect the workspace' });
  await runner.waitUntil(count('turn_end', 1), 'the inspect turn to finish');

  const factLines = Object.fromEntries(readFileSync(facts, 'utf8').trim().split('\n').map((line) => [line.slice(0, line.indexOf('=')), line.slice(line.indexOf('=') + 1)]));
  assert.equal(factLines.GROK_FOLDER_TRUST, '1', 'the child runs with the gate forced on');
  assert.equal(factLines.trust_store, 'absent', 'the fresh GROK_HOME carries no recorded grant');
  assert.equal(factLines.stdin, 'pipe', 'headless: stdin is not a TTY');
  assert.equal(factLines.stderr, 'pipe', 'headless: stderr is not a TTY');
  assert.ok(!factLines.argv.includes('--trust'), 'the runner never passes --trust');

  const report = JSON.parse(readFileSync(inspectJson, 'utf8'));
  assert.equal(report.projectTrusted, false, 'grok itself resolves the workspace untrusted');
  const inWorkspace = (path) => typeof path === 'string' && path.startsWith(workspace);
  assert.deepEqual(report.hooks.filter((h) => inWorkspace(h.source?.path)), [], 'no project hooks load');
  assert.ok(!report.skills.some((s) => inWorkspace(s.source?.path) || s.name === 'evil-skill'), 'no project skills load');
  assert.ok(report.plugins.filter((p) => inWorkspace(p.path)).every((p) => p.enabled === false), 'project plugins stay disabled');
  // `inspect` lists project MCP servers even when untrusted — it is a discovery report, and 1.0.34
  // exposes no per-server trust verdict for them. The gate that matters is at spawn: grok drops
  // project-scoped servers for an untrusted workspace before any process starts (upstream
  // session/managed_mcp.rs → folder_trust::filter_untrusted_project_mcp_with), keyed on the same
  // projectTrusted verdict asserted above.
  assert.ok(Array.isArray(report.mcpServers), 'the discovery report came back parseable');

  await stop(runner);
});
