import assert from 'node:assert/strict';
import test from 'node:test';

import { runPreflight, scanCommand, scanMode, shouldBlock } from '../preflight.mjs';

const collect = () => {
  const events = [];
  return { events, emit: (e) => events.push(e) };
};

/** A scanner that prints `out`, then exits with `code`, using the real shell-free spawn shape. */
const fakeScanner = (out, code) => (cmd, args, opts) => {
  const listeners = { close: [], error: [] };
  const streams = {
    stdout: { on: (_, fn) => out && fn(Buffer.from(out)) },
    stderr: { on: () => {} },
    on: (name, fn) => listeners[name]?.push(fn),
    kill: () => {},
  };
  queueMicrotask(() => listeners.close.forEach((fn) => fn(code)));
  streams.seen = { cmd, args, opts };
  return streams;
};

test('mode defaults to off and rejects anything unrecognised', () => {
  assert.equal(scanMode({}), 'off');
  assert.equal(scanMode({ COLONIZER_SCAN: '' }), 'off');
  assert.equal(scanMode({ COLONIZER_SCAN: 'yes' }), 'off');
  assert.equal(scanMode({ COLONIZER_SCAN: 'WARN' }), 'warn');
  assert.equal(scanMode({ COLONIZER_SCAN: ' block ' }), 'block');
});

test('no command means no scan, whatever the mode says', () => {
  assert.equal(scanCommand({ COLONIZER_SCAN: 'block' }).argv, null);
  assert.deepEqual(scanCommand({ COLONIZER_SCAN: 'warn', COLONIZER_SCAN_COMMAND: 'agentshield scan .' }).argv, [
    'agentshield',
    'scan',
    '.',
  ]);
});

test('off runs nothing at all', async () => {
  const { events, emit } = collect();
  const spawnFn = () => assert.fail('a scanner must not run when scanning is off');
  const result = await runPreflight({ env: {}, emit, spawnFn });
  assert.equal(result.ran, false);
  assert.equal(events.length, 0, 'off should not even log');
});

test('warn reports findings and lets the colony continue', async () => {
  const { events, emit } = collect();
  const result = await runPreflight({
    env: { COLONIZER_SCAN: 'warn', COLONIZER_SCAN_COMMAND: 'scan .' },
    emit,
    spawnFn: fakeScanner('prompt injection in README.md\n', 2),
  });
  assert.equal(result.ran, true);
  assert.equal(result.code, 2);
  assert.equal(shouldBlock(result), false, 'warn must never stop a colony');
  assert.match(events.at(-1).message, /prompt injection in README\.md/);
  assert.equal(events.at(-1).level, 'warn');
});

test('block stops the colony only on a non-zero exit', async () => {
  const clean = await runPreflight({
    env: { COLONIZER_SCAN: 'block', COLONIZER_SCAN_COMMAND: 'scan .' },
    emit: collect().emit,
    spawnFn: fakeScanner('', 0),
  });
  assert.equal(shouldBlock(clean), false);

  const dirty = await runPreflight({
    env: { COLONIZER_SCAN: 'block', COLONIZER_SCAN_COMMAND: 'scan .' },
    emit: collect().emit,
    spawnFn: fakeScanner('found something\n', 1),
  });
  assert.equal(shouldBlock(dirty), true);
});

test('a scanner that cannot start never blocks a colony', async () => {
  // An operator's broken scanner must not become an outage across every colony.
  const { events, emit } = collect();
  const result = await runPreflight({
    env: { COLONIZER_SCAN: 'block', COLONIZER_SCAN_COMMAND: 'no-such-scanner' },
    emit,
    spawnFn: () => {
      throw new Error('ENOENT');
    },
  });
  assert.equal(result.ran, false);
  assert.equal(shouldBlock(result), false);
  assert.match(events.at(-1).message, /did not run/);
});

test('the command is split without a shell', async () => {
  let seen = null;
  await runPreflight({
    env: { COLONIZER_SCAN: 'warn', COLONIZER_SCAN_COMMAND: 'scan . && curl evil.example' },
    emit: collect().emit,
    spawnFn: (cmd, args) => {
      seen = { cmd, args };
      return fakeScanner('', 0)(cmd, args);
    },
  });
  // `&&` arrives as an argument, not as a second command.
  assert.equal(seen.cmd, 'scan');
  assert.deepEqual(seen.args, ['.', '&&', 'curl', 'evil.example']);
});

test('findings are capped so a colony log cannot be flooded', async () => {
  const { events, emit } = collect();
  await runPreflight({
    env: { COLONIZER_SCAN: 'warn', COLONIZER_SCAN_COMMAND: 'scan .' },
    emit,
    spawnFn: fakeScanner('x'.repeat(50000), 1),
  });
  assert.ok(events.at(-1).message.length < 21000, 'output should be truncated');
});
