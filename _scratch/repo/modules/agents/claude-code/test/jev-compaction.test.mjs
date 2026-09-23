import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { AsyncQueue, buildOptions, jevVersionOk, runAgent } from '../runner.mjs';

const fixed = (...messages) =>
  async function* () {
    for (const m of messages) yield m;
  };

/** A fake Agent SDK `query` that plays `turnFn`, with the protocol drip needed to shut down. */
async function run(turnFn, options) {
  const query = () => {
    const generator = turnFn();
    generator.interrupt = async () => {};
    generator.close = () => {};
    return generator;
  };
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'user_message', text: 'go' });
  const emit = (e) => {
    events.push(e);
    if (e.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  await runAgent({ query, commands, emit, options, graceMs: 100 });
  return events;
}

const jevOptions = (extra = {}) =>
  buildOptions({ COLONIZER_DELEGATE: 'off', COLONIZER_JEV_COMPACTION: 'true', ...extra }).options;
const okInit = {
  type: 'system',
  subtype: 'init',
  session_id: 's1',
  model: 'm',
  claude_code_version: '2.1.280 (Claude Code)',
  plugins: [{ name: 'fast-jev-compaction', path: '/opt/colonizer/jev-compaction' }],
};
const done = { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
const logs = (events, level) => events.filter((e) => e.type === 'log' && (!level || e.level === level));

test('jev compaction is off by default, and a stale function-hooks flag is dropped', () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off' });
  assert.equal(options.plugins, undefined);
  assert.equal(options.settings, undefined);
  assert.equal(options.debugFile, undefined);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, undefined);

  const stale = buildOptions({ COLONIZER_DELEGATE: 'off', CLAUDE_CODE_ENABLE_FUNCTION_HOOKS: '1' });
  assert.equal(stale.options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, undefined);
});

test('jev compaction loads the plugin, sets the flag, and parses its numbers', () => {
  const options = jevOptions({ COLONIZER_JEV_KEEP_THRESHOLD: '0.7', COLONIZER_JEV_PRESERVE_RECENT: '10' });
  assert.deepEqual(options.plugins, [{ type: 'local', path: '/opt/colonizer/jev-compaction' }]);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
  assert.deepEqual(options.settings.pluginConfigs['fast-jev-compaction@inline'].options, {
    keepThreshold: 0.7,
    preserveRecentMessages: 10,
  });
  assert.ok(options.debugFile.startsWith(tmpdir()));

  const bad = jevOptions({ COLONIZER_JEV_KEEP_THRESHOLD: 'high' });
  assert.deepEqual(bad.settings.pluginConfigs['fast-jev-compaction@inline'].options, {});
  const custom = jevOptions({ COLONIZER_JEV_COMPACTION_DIR: '/mnt/jev' });
  assert.deepEqual(custom.plugins, [{ type: 'local', path: '/mnt/jev' }]);
});

test('jev compaction composes with rtk', () => {
  const options = jevOptions({ COLONIZER_RTK: 'true' });
  assert.ok(options.env.PATH.startsWith('/opt/colonizer/bin:'), options.env.PATH);
  assert.ok(options.hooks.PreToolUse.some((e) => e.matcher === 'Bash'), 'the rtk Bash hook is still there');
  assert.deepEqual(options.plugins, [{ type: 'local', path: '/opt/colonizer/jev-compaction' }]);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
});

test('jev compaction leaves the Headroom router URL alone', () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off', COLONIZER_JEV_COMPACTION: 'true' }, { routerUrl: 'http://127.0.0.1:9999' });
  assert.equal(options.env.ANTHROPIC_BASE_URL, 'http://127.0.0.1:9999');
  assert.ok(options.plugins.some((p) => p.path === '/opt/colonizer/jev-compaction'));
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
});

test('the version check tolerates suffixes and patch versions', () => {
  assert.ok(jevVersionOk('2.1.274'));
  assert.ok(jevVersionOk('2.1.280 (Claude Code)'));
  assert.ok(jevVersionOk('2.2'));
  assert.ok(!jevVersionOk('2.1.100'));
  assert.ok(!jevVersionOk(undefined));
});

test('init warns on an old Claude Code and on a missing plugin', async () => {
  const options = jevOptions();
  const old = await run(fixed({ ...okInit, claude_code_version: '2.1.100', plugins: [] }, done), options);
  assert.match(logs(old, 'warn')[0].message, /needs Claude Code 2\.1\.274 or later.*2\.1\.100.*built-in summary/);

  const missing = await run(fixed({ ...okInit, plugins: [{ name: 'other', path: '/x' }] }, done), options);
  assert.match(logs(missing, 'warn')[0].message, /didn't load the fast-jev-compaction plugin/);

  const ok = await run(fixed(okInit, done), options);
  assert.deepEqual(logs(ok, 'warn'), []);
});

test('a verdict written before init is ignored; one written after is reported', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    writeFileSync(log, '[debug] kept 1/10 messages, no summary (stale)\n');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const compact = (compact_metadata) => ({ type: 'system', subtype: 'compact_boundary', compact_metadata });
    const events = await run(async function* () {
      yield okInit;
      yield compact({ trigger: 'auto', pre_tokens: 1, post_tokens: 2 });
      appendFileSync(log, '[debug] kept 120/300 messages, no summary (fresh)\n');
      yield compact({ trigger: 'auto', pre_tokens: 3, post_tokens: 4 });
      yield done;
    }, options);
    const compactions = logs(events, 'info').map((e) => e.message).filter((m) => m.startsWith('Compaction'));
    assert.equal(compactions.length, 2);
    assert.match(compactions[0], /left no verdict/);
    assert.ok(!compactions[0].includes('stale'), 'the pre-init verdict is not re-reported');
    assert.match(compactions[1], /kept 120\/300 messages, no summary/);
    assert.ok(!compactions[1].includes('stale'), 'only the new bytes are read');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('compact_boundary reports the plugin verdict from the debug log', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const compact = (compact_metadata) => ({ type: 'system', subtype: 'compact_boundary', compact_metadata });
    const events = await run(async function* () {
      yield okInit;
      writeFileSync(log, 'noise\n[debug] $.ui.toast: kept 120/300 messages, no summary (score 0.42)\n');
      yield compact({ trigger: 'auto', pre_tokens: 90000, post_tokens: 12000 });
      rmSync(log); // a rotated-away log reads as no verdict, never an error
      yield compact({ trigger: 'manual', pre_tokens: 50000 });
      writeFileSync(log, 'fallback to built-in summary (hook timed out)\n');
      yield compact({ trigger: 'auto', pre_tokens: 80000, post_tokens: 80000 });
      yield done;
    }, options);
    const compactions = logs(events, 'info').map((e) => e.message).filter((m) => m.startsWith('Compaction'));
    assert.equal(compactions.length, 3);
    assert.match(compactions[0], /Compaction \(auto, 90000 → 12000 tokens\): kept 120\/300 messages, no summary/);
    assert.match(compactions[1], /Compaction \(manual, 50000 → \? tokens\): fast-jev-compaction left no verdict/);
    assert.match(compactions[2], /fallback to built-in summary/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('with jev off, compact_boundary stays ignored', async () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off' });
  const compact = { type: 'system', subtype: 'compact_boundary', compact_metadata: { trigger: 'auto', pre_tokens: 1, post_tokens: 2 } };
  const events = await run(fixed(okInit, compact, done), options);
  assert.ok(!logs(events).some((e) => e.message.startsWith('Compaction')));
});
